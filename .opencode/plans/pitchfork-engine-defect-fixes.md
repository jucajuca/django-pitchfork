# pitchfork-engine — Defect Fixes Before the Replacement Plan

Fixes for the defects found while reviewing
`haystack-replacement-plan.md`. Every item below was reproduced against the
shipped engine (embedded + server modes) before being written down; nothing
here is speculative.

**Why this plan exists.** The replacement plan's Phase 1 ("drop haystack from
`pitchfork.backends.base`") and Phase 2 (IR completion) are only worth doing on
an engine whose V1 behaviour actually matches haystack. It does not, on seven
paths that haystack users hit routinely (`auto_query`, `filter(content=...)`,
`__contains`, faceting, sorting, highlighting, bare lookups on typed fields).
Porting the conformance suite onto today's engine would encode these bugs as
expected results.

**Ordering.** Fix P0 first, in this repo, as a `pitchfork-engine` patch/minor
release. The replacement plan's Phase 1 must then pin `>=` that release.

## Summary

| ID | Defect | Worst symptom | Area |
|---|---|---|---|
| P0-1 | `_input_value` reads `.value` | `auto_query()`/`raw_search()` raise `AttributeError` | Python |
| P0-2 | `content` pseudo-field unmapped | `filter(content=…)` fails; silently empty result | Python |
| P0-3 | `FilterOp::Content` is text-only | bare lookup on int/bool/date returns 0 | Rust |
| P0-4 | `Contains` lowered as `Exact` | `__contains` does nothing (substring) | Rust |
| P0-5 | Facets computed over the page | facet counts change with pagination | Rust |
| P0-6 | Snippet map keyed wrongly | highlighting never returns anything | Rust |
| P0-7 | No fast fields on text types | `order_by("name")` returns no results | Rust |
| P1-1 | `boost` dropped | relevance control silently ignored | Python |
| P1-2 | `distance_point` ignored | no `_distance` annotation | Python |
| P1-3 | `validate()` never called | bad ASTs fail late/generically | Rust |
| P1-4 | `/index` before `/setup` | errors swallowed, always acks ok | Rust |
| P1-5 | Uneven wire-version checks | embedded mode skips negotiation | Both |
| P1-6 | `_process_results` needs haystack | blocks the haystack-free gate | Python |
| P1-7 | Multi-key sort ignored | second sort key silently dropped | Rust |
| P2-1 | `__in` with `date` on DateTimeField | returns 0 vs. `datetime` value | Rust |
| P2-2 | Field named `score` | `TypeError` constructing results | Python |
| P2-3 | No LICENSE file | packaging/legal | — |

---

## Confirmed defects

Severity: **P0** = wrong results or a crash on a normal haystack call;
**P1** = silently ignored feature or a robustness hole; **P2** = edge case or
packaging.

### P0-1 · `_input_value` uses a non-existent attribute

`python/pitchfork/backends/base.py:109-113`

```python
if isinstance(value, (AutoQuery, Clean, Exact, Raw)):
    return value.value          # ← haystack inputs carry .query_string
```

Haystack's input classes (`haystack/inputs.py`) all store `.query_string`;
`BaseInput.__init__(self, query_string, **kwargs)` is the only storage. There is
no `.value` anywhere in `inputs.py`.

Reproduced: `sqs.auto_query("daniel")`, `sqs.raw_search("daniel")`,
`filter(name=Exact("daniel"))` and `filter(name=Clean("daniel"))` each raise
`AttributeError: 'AutoQuery'/'Raw'/'Exact'/'Clean' object has no attribute 'value'`.

A second defect sits next to it: in `encode_filter` the branch
`if isinstance(value, AutoQuery)` can never be true, because `_input_value`
has already unwrapped the input on the line before. The `Clean/Exact/Raw`
re-unwrap a few lines later repeats the same `.value` mistake.

**Fix.** Unwrap with `.query_string`; keep the input object (or its
`input_type_name`) so the `AutoQuery` → `content`-op branch can still fire;
delete the dead re-wrap.

**Tests.** `auto_query` with plain text, with a quoted phrase, and with a
leading `-term`; `raw_search`; `filter(field=Exact(...))`, `Clean(...)`,
`Raw(...)`; `Not(...)` inside `SQ`.

### P0-2 · The `content` pseudo-field is never translated

`base.py::encode_filter` / `build_ast`.

`filter(content="…")` and `auto_query()` both encode the literal field name
`content`, which is not in the index schema. Reproduced: the server answers
`query error: unknown field 'content' in index schema`; with the default
`SILENTLY_FAIL = True` the caller sees an empty result set and no exception.

Every haystack backend special-cases this: `whoosh_backend.py:990` and
`solr_backend.py:776` both do

```python
if field == "content":
    index_fieldname = ""      # "no special field"
```

The adapter already computes the right target in
`build_schema_definition` (`content_field_name`), and both backends store it as
`self.content_field_name`, but `encode_filter` never consults it.

**Fix.** Map `field == "content"` (and any empty field) to
`self.content_field_name` at encode time. Because `encode_filter` is a module
function without backend state, either take the name as an argument from
`build_query`/`build_ast`, or move the mapping into
`PitchforkSearchQuery.build_query` where `self._using` is available.

**Tests.** `filter(content="term")` and `auto_query("term")` both return the
expected documents on a document-field index; and both modes (embedded,
server).

### P0-3 · Bare lookups on typed fields match nothing

*Not raised in the review; found while verifying it.*

`filter(rank=2)` (IntegerField), `filter(is_active=True)` (BooleanField) and
`filter(pub_date=<datetime>)` (DateTimeField) all return **0**, while
`filter(rank__exact=2)` returns the correct row. Reproduced on a three-document
index for all three field types.

Cause: a bare expression has no `__lookup`, so `split_expression` yields
`filter_type = "content"` (correct — this mirrors haystack), and
`FilterOp::Content` in `crates/pitchfork-engine/src/lib.rs:779` is implemented
only for text:

```rust
if !is_document {
    return Ok(Box::new(TermQuery::new(
        Term::from_field_text(tfield, &words),   // ← text term on an i64/date field
        basic,
    )));
}
```

`Term::from_field_text` on a numeric/date field cannot match, so the query is
silently empty rather than an error.

**Fix.** In the `Content` branch, dispatch on `FieldDef.field_type` the same way
`Exact` does: numeric/date/bool → the typed term (reuse `type_term`), text →
today's tokenized behaviour. `FilterOp::Content` on a non-document field then
becomes `Exact` for typed fields, which is what haystack's `%s` fragment does
after `_from_python` coercion.

**Tests.** Bare lookup vs `__exact` for every field type in the engine's own
Rust test module (one table-driven test), plus a Python round-trip per type.

### P0-4 · `__contains` is compiled as `__exact`

`crates/pitchfork-engine/src/lib.rs:647`

```rust
FilterOp::Exact | FilterOp::Contains => match ty { … }   // identical lowering
```

Haystack's semantics for every backend is a wildcard substring:

- whoosh: `"contains": "*%s*"` (`whoosh_backend.py:998`)
- solr: `"contains": "*%s*"` (`solr_backend.py:784`)

Reproduced: `filter(name__contains="ani")` returns 0 where haystack returns the
one document `anita`; `filter(name__contains="anita")` returns 1, which is why
the current test passes — it only ever asserts the whole-value case
(`pitchfork_tests/test_backend.py:94` uses `text__contains="Indexed"`, a
whole token).

**Fix.** Lower `Contains` to a substring match: `RegexQuery` over the raw
field (`.*<escaped>.*`), matching the existing `StartsWith`/`EndsWith` pattern
at lines 709–722 and reusing `regex_escape`. `Exact` keeps the typed
`TermQuery`. Note the cost: `RegexQuery` is not cheap on large lexicons — keep
it off the hot path and document the difference in the engine README.

**Tests.** Substring at the start/middle/end of a token, case sensitivity,
multivalued field, and a regex-metacharacter value (`"a.b"`, `"a(b"`) to prove
`regex_escape` is applied.

### P0-5 · Facets are computed over the result page, not the match set

`crates/pitchfork-engine/src/lib.rs:1473-1510` (`aggregate_facets` iterates
`addresses`) and `search` at line 265, which passes the *page* addresses.

Reproduced: 6 documents, 3× `alpha` / 3× `beta`; a full search reports
`{alpha: 3, beta: 3}`, but `search(start=0, end=2, facets=…)` reports
`{alpha: 1, beta: 1}`.

Haystack's Whoosh backend groups the whole `raw_page.results` (a
`ResultsPage` over the full result set) and Solr facets server-side over all
matches, so counts are page-independent in haystack.

`FACET_SCAN_LIMIT = 10_000` in `base.py:36` is defined and never used — the
intent was a bounded full scan, and it is currently dead.

**Fix.** Run facet collection over all matches, bounded by a limit that the
client sends (wire the existing `FACET_SCAN_LIMIT` through, or add a
`facet_scan_limit` field to `SearchRequestV1` in the V2 work). Where the match
set exceeds the cap, return counts plus an explicit `truncated: true` flag
rather than silently wrong numbers.

**Tests.** Facet counts identical for `start/end` windows of 1, 2 and 10 on a
6-document index; counts unchanged by `order_by`.

### P0-6 · Highlighting can never return a snippet

`crates/pitchfork-engine/src/lib.rs:1512-1537` and 274-279.

`build_snippets` builds its map keyed by **field name**:

```rust
snippets.entry(field_name.clone()).or_default().push(snippet);
```

and `search` joins it keyed by **document id**:

```rust
if let Some(fragments) = snippets.get(&hit.id) { … }   // never matches
```

Reproduced: a `highlight` request returns hits with no `snippets` key at all,
and `result.highlighted` is `None`. (The review attributed this to
`SnippetGenerator::create` failing; the actual cause is the key mismatch. The
silent `if let Ok(generator)` is worth fixing anyway — it hides real errors.)

**Fix.** Key the map by `hit.id`; surface `SnippetGenerator::create` errors
(log at warn, or return them) instead of swallowing them; drop the per-hit
`TermQuery` re-search by collecting `DocAddress` during the main pass (the
current code does one extra search per hit).

**Tests.** `highlight()` on a document-field match returns a
`{field: [fragment]}` mapping containing the matched term; `{% highlight %}`
renders it. Both modes.

### P1-1 · `boost` is accepted and dropped

`python/pitchfork/backends/server.py:208-210` skips `doc["boost"]` at index
time; `SearchRequestV1` has no boost field, so
`SearchQuerySet.boost(term, value)` has no effect. Reproduced: the ordering with
and without `.boost("anita", 100.0)` is identical.

`FieldDef.boost` exists in the wire schema (`schema.rs:72`) and is likewise
never applied to any query.

Until Phase 2 adds a `boost` IR op, this must warn: a silently ignored
relevance control is worse than an unsupported one.

**Fix.** `warnings.warn(...)` in `add_boost` (or in `search` when
`self.boost` is non-empty), plus a note in MIGRATION.md.

### P1-2 · `distance_point` is accepted and ignored

`server.py:285` / `embedded.py:217` take `distance_point` and never use it; no
`_distance` / `_point_of_origin` keys are added to results. Haystack's Solr and
ES backends both annotate (`solr_backend.py:560-570`,
`elasticsearch_backend.py:377-390`).

**Fix.** Warn when `distance_point` is passed (documented gap), and remove the
`distance`/`_distance` claim from MIGRATION.md's "what carries over" list.

### P1-3 · `QueryNode::validate()` is never called

`crates/pitchfork-core/src/query_ast.rs:111`. The only callers are its own unit
tests (lines 226–232). Neither `pitchfork-server`'s request handlers nor the
PyO3 entry points validate, so type errors (`filter(is_active="maybe")`) are
only caught if they happen to fail during lowering, and unknown fields surface
late with a generic message.

**Fix.** Call `.validate(&schema)` at the top of `TantivyEngine::search` (one
place, both modes) and map the error to the response `error` field.

**Tests.** A table of invalid nodes (unknown field, wrong value type per field
type) asserting a clean error instead of a silently empty result.

### P1-4 · `/index` before `/setup` swallows failures and always acks ok

`crates/pitchfork-server/src/main.rs:160-181`. The first-batch path discards
the indexing result:

```rust
let _ack = engine.index(&req).unwrap_or_else(|e| AckResponseV1::error(e.to_string()));
*state.engine.write().await = Some(Arc::new(engine));
Ok(Json(AckResponseV1 { ok: true, … }))     // ← always ok
```

Any document error (bad value for a field type, writer failure) becomes silent
data loss. The same path also registers the engine even when indexing failed.

**Fix.** Propagate `_ack`'s error; only store the engine after a successful
index; return the engine-computed fingerprint rather than re-deriving it.

### P1-5 · Wire-version checks are uneven

- PyO3 (`src/lib.rs:52-81`): `search`, `remove` and `clear` never compare
  `req.v`; only `index` does (and `clear` takes no request at all). Server mode
  checks all four (`main.rs:137-209`).
- `python/pitchfork/backends/server.py` sends hardcoded `"v": 1` in five
  requests (`update`, `remove`, `search`, `more_like_this`, `setup`) instead of
  `wire_v1.WIRE_VERSION`. A future bump would silently leave server mode
  speaking V1.

**Fix.** Move the version check into `TantivyEngine` (so both transports inherit
it), give `clear` a real request type, and replace every `"v": 1` literal with
`wire_v1.WIRE_VERSION`. Add a test that asserts the literal appears nowhere
outside `wire.py`.

### P1-6 · `_process_results` requires haystack at query time

`python/pitchfork/backends/server.py:47-68` does
`from haystack import connections`, reads `UnifiedIndex`, and calls
`haystack_get_model`. This is the concrete blocker for the replacement plan's
Phase 1 ("engine runs with haystack uninstalled") — the review is right that
the plan understates it. See the replacement plan's revised Phase 1 for the
ownership decision (engine emits raw hits; the replacement resolves models).

**Fix in this repo:** make the model/index accessor injectable — a small
protocol (`resolve_model(app_label, model_name)`, `field_converters()`) that
defaults to the haystack-backed implementation and can be replaced. Then the
engine can pass a haystack-free test with a stub accessor. Removing the
`haystack` imports entirely remains Phase 1 of the replacement plan.

### P2-1 · `__in` on a DateTimeField with `date` values returns 0

Reproduced: `pub_date__in=[datetime(2020,1,1,12,0,0)]` → 1;
`pub_date__in=[date(2020,1,1)]` → 0. Haystack normalizes via
`_convert_datetime` (`whoosh_backend.py:1070`, `solr_backend.py`), so a `date`
works. `FilterOp::In` only special-cases `FieldType::Date`, and
`wire_to_date` on a date-only string yields midnight for a `date_time` field.

**Fix.** In the `In`/`Range` branches, when the target field is
`FieldType::DateTime` and the value is a date-only string, widen to the whole
day (the same helper `Exact` already uses for `FieldType::Date`).

### P2-2 · An index field named `score` crashes result construction

Reproduced: an index with `score = indexes.FloatField()` raises
`TypeError: SearchResult.__init__() got multiple values for argument 'score'`
because `_process_results` splats stored fields into
`result_class(app_label, model_name, pk, score, **additional_fields)`.

Haystack's own backends have the same latent collision, so this is not a
regression — but the adapter can fail better than a `TypeError`.

**Fix.** Pop haystack's reserved `SearchResult` names (`score`,
`app_label`, `model_name`, `pk`, `_distance`, `_point_of_origin`) out of
`additional_fields` before splatting and log a warning naming the field.

### P2-3 · No LICENSE file

`pyproject.toml` declares `license = { text = "BSD-3-Clause" }` and the
`License :: OSI Approved :: BSD License` classifier, but the repo has no
`LICENSE` file. Add one (and the matching classifier text).

### P0-7 · Sorting on a text/char field silently returns no results

`crates/pitchfork-engine/src/lib.rs:510-521` sorts string fields with
`order_by_string_fast_field`, but `build_tantivy_schema` (lines 1028–1059)
never calls `.set_fast()` on `Text`, `Char`, `Ngram` or `EdgeNgram` fields —
only the numeric/date branches do (lines 1064–1085). Tantivy therefore rejects
the request with "Field 'name' is not configured as fast field"; the server
returns an error response, and with the default `SILENTLY_FAIL = True` the
adapter converts it into an empty result set.

Reproduced: `order_by("name")` and `order_by("-name")` both return `[]` on an
index where every document has a name. Integer/float/date/bool sorting works,
which is why the existing tests pass — `pitchfork_tests/test_backend.py:141`
and `:148` only sort on `pub_date`.

**Fix.** Call `.set_fast()` on the four text field types (index-size cost is
real but sorting requires it; whoosh and Solr both pay an equivalent cost), and
make the collector's failure loud rather than an empty page.

**Tests.** Ascending and descending sort on a char field, a multivalue field,
and a mixed `order_by("pub_date", "-name")` (multi-field semantics are a
separate question — see P1-7 below).

### P1-7 · Multi-field sort silently ignores everything but the first key

`lib.rs:478`: `let (field, order) = parse_sort(&sort[0])?;` — with the comment
"V1 sorts by the first field only (whoosh has the same all-or-nothing rule)".
The *comment* describes whoosh's direction-consistency rule, not a licence to
drop keys: haystack's Whoosh backend accepts multiple `order_by` fields and
raises only when directions are mixed
(`whoosh_backend.py:434-444`).

Reproduced: `order_by("views", "-name")` returns two documents ordered by
`views` alone; the second key has no effect and no warning is emitted.

**Fix.** Either implement multi-key sort via a chained collector, or reject
`len(sort) > 1` with an explicit error instead of ignoring the tail. Add
whoosh's mixed-direction check.

**Tests.** Multi-key sort with consistent directions; mixed directions raise.

---

## Not defects (review claims rejected)

Recording these so they are not re-litigated:

| Review claim | Status |
|---|---|
| `docs/backend_support.rst` is "plain prose, not the authoritative matrix" | **Wrong.** `django-haystack/docs/backend_support.rst:99-116` is a full 8-column "Backend Support Matrix" table. The plan's citation is accurate. |
| The plan claims "core tests = 170 LOC" | **Wrong.** The plan never states that. 170 LOC is the size of `test_haystack/core/`, which the plan does not cite. |
| `base.py:30` re-exports are entirely unused (`# noqa: F401`) | **Partly right, low impact.** `spatial_filter` *is* used by both adapters; `encode_point`, `spatial_within`, `spatial_dwithin` are not used anywhere outside `wire.py` (tests import them from `wire` directly). Narrow the re-export to `spatial_filter` (or drop it and import from `wire`). |
| Highlighting fails because `SnippetGenerator::create` errors | **Wrong cause.** It is the `snippets` map key mismatch (P0-6). The silenced `if let Ok` should still be fixed. |
| `create_spelling_suggestion` returning `None` is a bug | **Not a bug.** Spelling travels in the response payload; the haystack method is a secondary path. |

---

## Phases

### Phase A — P0 correctness (engine repo, one release)

1. P0-1 `_input_value` + dead branch (Python only).
2. P0-2 `content` → `content_field_name` mapping (Python only).
3. P0-3 `FilterOp::Content` type dispatch (Rust).
4. P0-4 `Contains` → substring `RegexQuery` (Rust).
5. P0-5 page-independent facets + wire `facet_scan_limit` (Rust).
6. P0-6 snippet key fix + error surfacing + no per-hit re-search (Rust).
7. P0-7 fast fields on text/char field types (Rust) — note this changes the
   index format, so it must ship as a **rebuild-required** release: document
   that adopters must run `rebuild_index` (a schema fingerprint change already
   errors loudly, which is the correct behaviour here).

Gate: every defect above has a regression test that fails on the current
engine. Ship as a `pitchfork-engine` minor (`0.x+1`) with a changelog entry
naming the semantic changes (`__contains` now means substring; facets are
page-independent; sorting on text fields now works and requires a rebuild).

### Phase B — P1 loud failures (engine repo, same release if cheap)

7. P1-1 boost warning; P1-2 distance warning.
8. P1-3 `validate()` on the search entry point.
9. P1-4 `/index` error propagation.
10. P1-5 wire-version checks in `TantivyEngine` + kill `"v": 1` literals.
11. P1-6 injectable model/index accessor (no behaviour change; enables the
    replacement plan's Phase 1 gate).
12. P1-7 multi-key sort (implement or reject loudly; add the mixed-direction
    check).

### Phase C — P2 polish

13. P2-1 `__in` date normalization.
14. P2-2 reserved-name guard in `_process_results`.
15. P2-3 LICENSE file.

### Phase D — Test suite hardening

The suite is 763 LOC across 31 tests, with no coverage of any defect above.
Add, minimum:

- `test_inputs.py` — AutoQuery/Raw/Clean/Exact/Not round-trips.
- `test_content_field.py` — `filter(content=…)`, `auto_query`, `raw_search`.
- `test_filter_ops.py` — table-driven per-op × per-field-type (exact,
  contains, startswith, endswith, gt/gte/lt/lte, in, range, fuzzy, bare), the
  same table run in embedded and server modes.
- `test_facets.py` — page-independence, multivalue, truncation flag.
- `test_sorting.py` — char/int/bool/date, ascending and descending, multi-key,
  and the mixed-direction rule.
- `test_highlighting.py` — snippets present and containing the term.
- `test_wire_protocol.py` — version mismatch rejected in all four operations
  and in both transports.

Run the whole thing twice (`PITCHFORK_MODE=embedded` and unset), as
`test_spelling.py` already does.

---

## Acceptance criteria

- Every P0 defect has a test that fails before the fix and passes after.
- The haystack-parity behaviours in this document match
  `django-haystack/haystack/backends/whoosh_backend.py` (the reference used
  throughout) for: `content` mapping, `contains`/`startswith`/`endswith`,
  `__contains` substring semantics, bare-lookup-on-typed-field, facet
  page-independence, text-field sorting, and `__in` with `date`.
- No feature is accepted-and-ignored without a warning (boost, distance,
  date/query facets, narrow queries).
- `grep -r '"v": 1' python/` returns nothing.
- `FACET_SCAN_LIMIT` is either wired through to the server or deleted.
- Test suite grows to cover the table above; both modes green.

## Out of scope

Phase 2 of the replacement plan (IR ops for `boost`/`stats`/`date_facets`/
`query_facets`/`autocomplete`/MLT params, wire V2) — that is planned there and
must land *after* this work, not concurrently.
