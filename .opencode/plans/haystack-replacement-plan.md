# django-pitchfork as a Complete Haystack Replacement — Plan

A full reimplementation of django-haystack's public API under the
**`django-pitchfork`** package, with first-class backends for every engine
haystack supports — **the native pitchfork engine (Rust/Tantivy), Solr,
Elasticsearch, Whoosh, Xapian** — plus the `simple` mock backend. Adopters
replace `haystack` with `django_pitchfork`; the API, templatetags, forms,
views, and management commands match 1:1.

## Two packages, not one

This project is **two separately released packages**, and every naming
decision below depends on keeping them straight:

| | Engine (this repo) | Replacement (this plan) |
|---|---|---|
| Package | **`pitchfork`** | **`django-pitchfork`** |
| Import | `import pitchfork` | `import django_pitchfork` |
| PyPI | `pitchfork-engine` | `django-pitchfork` |
| Contains | Rust/Tantivy core, wire codec, native backend, `backends.base` machinery | `SearchQuerySet`, fields, indexes, views, forms, templatetags, commands, external-engine backends |
| Depends on | Django + (currently) haystack for base classes | **`pitchfork-engine`** |
| Status | **shipped** (v0.1.0, this repo) | planned (this document) |

```
        django-pitchfork            ← the haystack replacement (this plan)
        (django_pitchfork)
                │  depends on
                ▼
        pitchfork-engine            ← the engine (this repo, shipped)
        (pitchfork)
                │  talks to
                ▼
        pitchfork-server  or  in-process pitchfork._core
```

The **import direction is one-way and enforced**: `django_pitchfork` imports
`pitchfork`, never the reverse. The engine must remain usable without the
replacement — it is what `PitchforkEngine` adopts today, before any of this
plan exists.

Names are fixed by `docs/adr/0004-engine-name-and-package-split.md` (in the
engine repo), which supersedes ADR 0002's inverted split. Note the asymmetry:
PyPI's `pitchfork` is held by an unrelated abandoned project, which is why the
engine's *distribution* is `pitchfork-engine` while its *import* is
`pitchfork`.

Related (read first):
- `docs/adr/0004-engine-name-and-package-split.md` (engine repo) — the naming
  decision this plan implements.
- `django-haystack/.opencode/plans/django-pitchfork-architecture.md` — the
  design principles this plan inherits.
- `django-haystack/.opencode/plans/django-pitchfork-migration-plan.md` — the
  completed phases 0–6 (native Rust core already exists).
- `django-haystack/.opencode/plans/rust-migration-notes-from-django-bolt.md` —
  packaging/testing lessons applied throughout.

> The three `django-haystack/.opencode/plans/…` documents above are historical
> inputs and are **not present** in the current `django-haystack` checkout
> (verified: `ls ../django-haystack/.opencode/plans/` → no such directory).
> Their still-relevant content is restated inline below; treat the references
> as provenance, not as required reading.

## Evaluation: what haystack is, measured

- `haystack/` ≈ 9,000 LOC. Backend-agnostic machinery: `backends/__init__.py`
  (1,109 — `BaseEngine`, `BaseSearchBackend`, `SearchNode`/`SQ`,
  `BaseSearchQuery`, `connections`, `UnifiedIndex`), `query.py` (768 —
  `SearchQuerySet`, ~50 public methods), `fields.py` (562 — 19 field classes
  incl. the `Facet*` mixins), `inputs.py` (169 — `BaseInput`, `Raw`,
  `PythonData`, `Clean`, `Exact`, `Not`, `AutoQuery`, `AltParser`),
  `indexes.py` (548 — `SearchIndex`/`ModelIndex`/registration), plus
  `views.py`, `generic_views.py`, `forms.py`, `admin.py`, `panels.py`,
  `models.py` (`SearchResult`), `manager.py`, `signals.py`, `routers.py`,
  `templatetags/`, `templates/`, and 5 management commands.
- Each engine backend re-implements **~20 of the same methods**
  (`build_search_kwargs`, `build_query_fragment`, `clean`, `run`,
  `build_schema`, `_process_results`, `more_like_this`,
  `extract_file_contents`) against that engine's DSL, with no shared IR.
  Solr's copy is 977 lines; Elasticsearch's 1,129 (+ 1,045 across the
  version-split `es5`/`es7` files); Whoosh's 1,104. This duplication is the
  maintenance hole the replacement closes.
- Backend test suite ≈ 7,000 LOC, **copy-pasted per backend**
  (`test_haystack/whoosh_tests`, `solr_tests`, `elasticsearch_tests` share
  shape, not code). The shared `test_haystack/*.py` core tests are
  backend-agnostic and port nearly as-is.
- Backends haystack ships: Solr, Elasticsearch (3 version-split files),
  Whoosh, `simple`. **Xapian and PostgreSQL FTS are third-party** and
  unmaintained. Capability parity matrix lives in `docs/backend_support.rst`
  (Solr is the reference: everything incl. stats requests and Tika file
  extraction; Whoosh lacks MLT-native + stats + spatial).

## The one architectural bet: a single query IR

Haystack's backends each build engine-native query strings/DSLs directly from
`SearchQuery` state. The replacement makes the query IR — already built and
property-tested as `pitchfork-core`'s `QueryNode` AST — the **single source of
truth**:

```
SearchQuerySet / SearchQuery (django_pitchfork, Python)  — accumulates the IR directly
        │
        ▼
QueryNode AST (pitchfork-core, Rust, proptest'd)  — ONE serialization
        │
        ├── pitchfork native: the AST IS the wire (engine, shipped phases 0–6)
        ├── Solr:     IR → solr QP params   (q, fq, facet.*, hl.*, mlt, sort)
        ├── ES:       IR → elasticsearch DSL (bool, aggs, highlight, suggest)
        ├── Whoosh:   IR → whoosh.qparser query objects
        └── Xapian:   IR → xapian Query API (QueryParser + Query tree)
        │
        ▼
Results normalized to one result dict shape       — shared SearchResult
```

The IR lives **in the engine** (`pitchfork-core`), not in the replacement: the
replacement consumes a type it does not own. That is what keeps the arrow
one-way.

Consequences:
1. `build_query_fragment` per backend **disappears** — one `build_query_fragment`
   (IR-level, shared), then a small per-backend *lowering* module.
2. `_process_results` is centralized once; per-backend code only maps engine
   responses into the normalized shape.
3. Filter-op parity bugs (the `startswith`/`endswith`/`contains` class of
   drift) become single-site fixes with property tests at the IR, not six
   copies.
4. Feature gaps (stats on Whoosh, spatial on Whoosh) are explicit IR ops the
   backend declares unsupported — with warnings, matching haystack's behavior.

## Design principles (inherited + new)

Inherited from the architecture doc (all still hold):
1. Django stays in Python. 2. Versioned wire, both sides in the same commit.
3. Compile once at registration. 4. No GIL for native search/indexing.
5. Parsers get property tests on both sides. 6. CI matrices from classifiers.

New for the replacement:
7. **One conformance suite, parameterized over backends.** haystack's
   copy-pasted per-backend tests become a single suite run against every
   engine — a backend either passes it or documents a gap.
8. **IR-first.** No backend builds queries from `SearchQuery` state directly;
   everything lowers from the IR.
9. **External services are loud.** Like the engine's test suite: services we
   own (pitchfork-server, Whoosh/Xapian file-based) never skip; Solr/ES run in
   CI via docker compose and skip **with a loud summary line** locally when
   unreachable (inverting haystack's silent-skip problem flagged in its
   AGENTS.md).
10. **Two packages, one arrow.** `django_pitchfork` depends on `pitchfork`; the
    engine never imports the replacement. Packaging, CI and docs keep the two
    release trains independent — the engine ships on its own cadence and must
    stay installable alone.

## Package layout

**Separate repo, separate package.** The replacement is *not* a new directory
inside the engine repo — it is its own project (`django-pitchfork`) that
installs `pitchfork-engine` as a dependency. The engine repo gains nothing from
this plan.

```
django_pitchfork/                  # the haystack replacement (new repo; dist: django-pitchfork)
├── __init__.py                   # public API re-exports (indexes, query, fields...)
├── fields.py                     # port of haystack/fields.py (19 classes)
├── inputs.py                     # port incl. AltParser (per-backend hook)
├── query.py                      # SearchQuerySet / SearchQuery (IR-accumulating)
├── indexes.py                    # SearchIndex / ModelIndex / registration
├── models.py, manager.py, signals.py, routers.py
├── views.py, generic_views.py, forms.py, admin.py, panels.py
├── templatetags/                 # highlight, more_like_this, search tags
├── templates/                    # search/*.html (ported)
├── management/commands/          # update_index, rebuild_index, clear_index,
│                                 #   haystack_info, build_schema (per-backend)
├── utils/                        # loading (UnifiedIndex/ConnectionHandler),
│                                 #   app_loading, geo, highlighting, log
├── backends/
│   ├── __init__.py               # BaseEngine, BaseSearchBackend, SearchNode/SQ,
│   │                             #   shared IR build_query_fragment, connections —
│   │                             #   subclassing the engine's pitchfork.backends.base
│   ├── pitchfork.py              # wraps the pitchfork engine (native, shipped)
│   ├── whoosh.py                 # pure Python, no service
│   ├── solr.py                   # pysolr
│   ├── elasticsearch.py          # elasticsearch-py 7/8+ (no version split)
│   ├── xapian.py                 # python-xapian
│   ├── simple.py                 # mock/debug
│   └── lowering/                 # IR → DSL compilers (one module per engine)
└── compat/                       # haystack import shim (phase 8)
```

Note that `pitchfork.py` here is a *thin adapter over the engine's existing
`pitchfork.backends.*`* — the Rust core, wire codec and dual embedded/server
modes are already shipped in `pitchfork-engine` and are not re-implemented.

### Layering rule

The engine's `pitchfork.backends.base` — which today subclasses haystack's
`BaseEngine`/`BaseSearchBackend`/`BaseSearchQuery` — is the seam. Phase 1 moves
that machinery **down into the engine** so that the engine no longer requires
haystack at runtime, and `django_pitchfork` builds on it:

```
pitchfork.backends.base    (ENGINE repo; haystack-free after Phase 1)
        ▲  subclassed by
django_pitchfork.backends.* (replacement: pitchfork-native, whoosh, solr, es, xapian, simple)
        ▲
django_pitchfork.query / indexes / views / forms / templatetags
```

Ownership of the shared machinery, after Phase 1:

| Machinery | Lives in | Import path |
|---|---|---|
| `BaseEngine`, `BaseSearchBackend`, `BaseSearchQuery`, `SearchNode`/`SQ`, `connections`, `UnifiedIndex` | **engine** | `pitchfork.backends.base` |
| `constants`, `SearchResult`, model/identifier utils | **engine** | `pitchfork.constants`, `pitchfork.models`, `pitchfork.utils` |
| `SearchQuerySet` (public chainable API), fields, inputs, indexes, views, forms, templatetags, commands | **replacement** | `django_pitchfork.*` |
| Per-engine lowering + backends | **replacement** | `django_pitchfork.backends.*` |

The split is deliberate: the low-level plumbing haystack hides in
`backends/__init__.py` is engine-shaped (it is what the native backends already
subclass today), while everything a haystack user actually imports
(`SearchQuerySet`, `SearchIndex`, `{% load highlight %}`) belongs to the
replacement.

Consequences:

- The engine (`pitchfork`) loses its runtime haystack dependency; haystack
  becomes a **dev dependency of both repos** (conformance-suite test apps + the
  phase 8 parity test).
- The replacement never re-exports or vendors the Rust core: it calls the
  engine.
- `django_pitchfork` may require a *minimum* `pitchfork-engine` version; the
  engine never knows the replacement exists.

## Backend scope & pins

| Backend    | Client              | Pin (verify at impl)     | Service in CI |
|---|---|---|---|
| pitchfork (native) | `pitchfork-engine` (this project) | `>=0.1`            | own fixture (never skips) |
| Whoosh     | whoosh              | `>=2.7,<3` (unmaintained)| none (file-based) |
| Solr       | pysolr              | `>=3.9,<5`               | docker compose (solr:9, core `collection1`) |
| Elasticsearch | elasticsearch-py | `>=7.17,<9` (7/8 only)   | docker compose (es:8) — destructive teardown, never point at real clusters |
| Xapian     | xapian (pip bindings)| `>=1.4` + system libxapian | docker image with libxapian |
| simple     | —                   | —                        | none |

Elasticsearch: **one** backend for 7/8+ (haystack's 3-file version split is a
maintenance liability; the 8.x client covers 7.17+ servers within minor
compat rules — verified against the compose service). PostgreSQL FTS stays
out of scope (third-party in haystack too) — listed as future work.

## Phases

Ordering rationale: Whoosh first (pure Python, no service, fastest
conformance loop), then Solr (most complete reference backend), then ES,
then Xapian (bindings need system libs).

**Repo ownership.** Phases 1 and 2 are the only ones that touch the engine
repo (`pitchfork-engine`); every other phase is replacement-repo work. The two
engine phases are sequenced first because the replacement cannot subclass
`pitchfork.backends.base` (Phase 1) or express the full IR (Phase 2) until
they land and are released.

### Phase 0 — Foundations
- `django_pitchfork/` package skeleton + public-API re-export surface; pyproject
  entry (dist `django-pitchfork`; extras: `django-pitchfork[solr]`,
  `[elasticsearch]`, `[xapian]`; hard dependency `pitchfork-engine>=0.1`).
- Port the backend-agnostic `test_haystack/*.py` core tests as the
  conformance base; scaffold `conformance/` suite parameterized over
  backends (single `BASE_TESTCASES` list run per backend via `__subclasses__`
  or parametrize). Port the app-discovery edge-case apps too:
  `discovery/`, `multipleindex/` (routers), `altered_internal_names`,
  `app_with_hierarchy`, `app_without_models`, `app_using_appconfig` — these
  are the UnifiedIndex registration edge cases.
- **`BackendCaps` mechanism (review finding F3)**: each backend ships a caps
  module declaring (a) supported IR ops (unsupported → the suite skips with
  a loud warning) and (b) per-backend expectation overrides (spelling
  suggestions, highlight fragment formats, tie-broken ordering differ by
  engine — the suite consults the caps rather than asserting one universal
  answer). The docs backend-support table is **generated** from the caps
  modules, so the table cannot drift.
- docker compose file at `docker/docker-compose.yml` (haystack's convention —
  not repo root) for solr + es + a xapian-capable image; loud-skip reporting
  in the runner.
- **Tests**: ported core tests green against `simple` backend.

### Phase 1 — Core API layer (Python, no engines)
- Port `fields.py` (all 19 classes, `convert`/`prepare` semantics),
  `inputs.py` (incl. `AltParser` as a per-backend hook), `SearchNode`/`SQ`
  (tree.Node semantics, `as_query_string` kept for `__str__`), `indexes.py`
  (registration, `SearchIndex.prepare`, `load_model_queryset`),
  `utils/loading.py` (`ConnectionHandler`, `UnifiedIndex`, deferred loading),
  `routers.py`, `models.py` (`SearchResult`), `manager.py`, `signals.py`.
- **Design change**: `BaseSearchQuery` accumulates the `QueryNode` IR
  directly (add_filter/add_order_by/add_boost/build_not_query...) instead of
  backend string fragments. Keep `__str__` via an IR pretty-printer.
- **Base-class move-down (review finding F1)**: `pitchfork.backends.base`
  (engine repo) currently imports `haystack.backends.*`, `haystack.constants`,
  `haystack.models` and `haystack.utils`. Phase 1 replaces those with
  engine-owned equivalents (`pitchfork.base`, `pitchfork.constants`,
  `pitchfork.models`, `pitchfork.utils`) so the **engine runs with haystack
  uninstalled**. `django_pitchfork` then subclasses those, and its own
  `haystack.*` vocabulary is filled by `django_pitchfork.*`.
- **Engine change, released separately**: together with Phase 2 this is one of
  the two engine-repo phases. It lands there first, as a `pitchfork-engine`
  minor version (dropping a runtime dependency does not change the backends'
  behaviour, but it *is* an API change for anyone importing
  `pitchfork.backends.base`, so call it out in the engine's changelog), and
  `django-pitchfork` pins `>=` that version.
- **Tests**: haystack's `test_fields.py`, `test_inputs.py`, `test_indexes.py`,
  `test_loading.py`, `test_query.py`, `test_managers.py` ported green; the
  native backend passes the conformance subset with haystack NOT installed.
  **Both repos assert this**: the engine's own suite gains a "haystack not
  importable" job, and `django_pitchfork`'s CI runs with haystack absent except
  in the phase 8 parity job.

### Phase 2 — IR completion + Wire V2 (engine repo)
- **Lands in the engine repo**, not the replacement: this is the second (and
  last) engine-side phase. Requires a `pitchfork-engine` minor bump and a
  `WIRE_VERSION` bump; the engine's own suite must stay green on V2 before
  `django-pitchfork` may depend on it.
- Extend `pitchfork-core/query_ast.rs` with the ops the native engine still
  lacks: `boost`, `stats`/`stats_facets`, `date_facets`, `query_facets`,
  `autocomplete`, MLT params (min_doc_freq/term fields), distance
  annotation. Wire **V2** (V1 → V2; both sides same commit, negotiation
  bumps per the compatibility contract).
- IR pretty-printer (for `str(query)`) + property tests both sides.
- Native engine implements the new ops where tantivy allows; declares the
  rest unsupported-with-warning (Solr-only `stats` on Whoosh-class engines).
- **Tests**: round-trip proptests; native conformance subset green on V2.

### Phase 3 — Whoosh backend
- `backends/whoosh.py`: schema build (per-field analyzers preserved —
  haystack's only per-field-analyzer backend), IR → `whoosh.qparser`
  lowering, native highlighting/facets/spelling, results → normalized shape.
- Declared gaps: MLT (approximate via terms), stats, spatial — warnings.
- **Tests**: full conformance suite green (no service needed); haystack's
  whoosh_tests quirks (spelling forms, fuzzy plugin) ported; verify whoosh
  2.7.4 on py3.12–3.15 before committing to the pin.

### Phase 4 — Solr backend
- `backends/solr.py`: pysolr; `build_solr_schema` command port (managed
  schema); IR → QP params lowering (q/fq/df/fl, facet.*, hl.*, mlt.*, sort,
  sppt); stats requests; Tika `extract_file_contents`; geo fieldType +
  spatial filter lowering; distance annotations via geodist().
- **Tests**: conformance suite against docker Solr; haystack's solr_tests
  (incl. `content_extraction` and `build_solr_schema` command tests) ported.

### Phase 5 — Elasticsearch backend
- `backends/elasticsearch.py`: one backend, elasticsearch-py 7/8+; IR → DSL
  lowering (`bool`/`query_string`, `aggs` for facets incl. date_histogram +
  filters, `highlight`, `suggest` for spelling, legacy `mlt` →
  `more_like_this` query); `geo_point` + `geo_distance`/`geo_bounding_box`;
  index templates instead of `build_schema` mappings API.
- **Version rule (review finding F4)**: an 8.x client talking to a 7.17
  server requires compat-mode headers and some API shapes differ. If compat
  mode proves painful against the compose-pinned server, target 8.x servers
  only and lower the documented floor — do not ship a three-file split.
- **Tests**: conformance suite against docker ES 8; destructive-teardown
  guard (refuse non-test index prefixes, mirroring haystack's AGENTS warning).

### Phase 6 — Xapian backend
- `backends/xapian.py`: python-xapian; IR lowering to QueryParser +
  programmatic `Query` tree; value-based sort/slots; facets via Xapian
  facet support; spelling via Xapian's spelling correction; MLT via
  `Enquire`-level similarity where available, else term-based.
- **Tests**: conformance suite in the xapian-capable image; system-lib
  required (loud-skip locally).

### Phase 7 — Views, forms, admin, templatetags, commands
- Port `views.py`, `generic_views.py`, `forms.py` (SearchForm/
  ModelSearchForm incl. selected_facets), `admin.py` + `panels.py`,
  `templatetags/` (`highlight`, `more_like_this`, `search` — tag **names
  unchanged** so templates port without edits), `templates/`,
  management commands (`update_index` with per-model workers,
  `rebuild_index`, `clear_index`, `haystack_info`, `build_schema` per
  backend replacing `build_solr_schema`).
- **Tests**: `test_views.py`, `test_generic_views.py`, `test_forms.py`,
  `test_templatetags.py`, `test_management_commands.py` ported; conformance
  suite covers each against ≥2 backends.

### Phase 8 — haystack compat shim
- **Mechanism (review finding F2)**: `from haystack.inputs import ...`
  cannot be redirected without an actual `haystack` distribution, and a shim
  named `haystack` collides with the real one. Decision: the shim lives at
  `django_pitchfork/compat/haystack/` and is used **explicitly** —
  `import django_pitchfork.compat.haystack as haystack` — never installed as
  `haystack`. The primary migration path remains a mechanical import-sed
  (`haystack.` → `django_pitchfork.`) documented in the replacement repo's
  migration guide; the shim covers gradual migrations only.
- Templatetag caveat: when haystack AND `django_pitchfork` are both in
  `INSTALLED_APPS`, Django resolves `{% load highlight %}` to the first app
  in list order — silent ambiguity. The migration guide says to remove
  haystack from `INSTALLED_APPS`; the replacement never coexists for reads.
- Ship the shim's own test: import-surface parity asserted against a frozen
  list of haystack's public names.
- **Tests**: parity test runs against the installed haystack (dev dep) and
  fails loudly when haystack grows new public names (forces review).

### Phase 9 — Hardening + release
- Conformance matrix in CI — **pruned (review finding F7)**: the full
  {py 3.10–3.15} × {django 4.2–6.1} matrix runs on the native backend only;
  every other backend runs one representative slice (one py, one django)
  plus its compose service. Full matrix ≈ 30 jobs, not 144. Loud-skip
  summary; the `pitchfork`-native backend also runs the dual-mode
  (embedded/server) matrix, driven from the engine's existing CI.
- Sphinx docs (`tox -e docs` style, warnings-as-errors) with the
  backend_support matrix regenerated from a source-of-truth table.
- PyPI release of `django-pitchfork` (dist name `django-pitchfork`, import
  `django_pitchfork`). Separate release train from `pitchfork-engine`:
  distinct repo, distinct tag namespace, `pitchfork-engine>=X` floor bumped
  deliberately. Reuses the engine's release-workflow pattern; extras for
  client libs.
- Benchmarks: conformance suite runtime per backend recorded
  (BENCHMARK.md pattern); the native backend remains the perf baseline.

## Non-goals

- **No changes to the engine's search behaviour or wire protocol.** The one
  engine-side change is Phase 1's haystack-import removal in
  `pitchfork.backends.base`; the `pitchfork.backends.server` /
  `pitchfork.backends.embedded` ENGINE paths and their options stay as shipped.
- **No merging of the two packages.** `django-pitchfork` is never vendored
  into this repo and `pitchfork-engine` never grows replacement code; the
  replacement depends on the engine, not the other way round.
- No haystack fork: haystack remains installed for the compat shim's parity
  tests only (a dev dependency of both repos).
- No PostgreSQL FTS backend (future work), no new engines beyond the table.
- No ORM/query integration beyond haystack's existing surface.

## Risk register

| Risk | Mitigation |
|---|---|
| haystack's undocumented `BaseSearchQuery` subtleties (`build_params`, `_process_results` result_class/distance, `SearchNode` deepcopy semantics) | Port tests first per module; conformance suite pins behavior |
| Whoosh unmaintained (2.7.x) | Pin `<3`; vendor decision deferred to a dedicated ADR if 2.7 breaks on a new Python |
| ES client 7/8 drift vs haystack's 3-file split | One backend, 7.17 floor; compose-pinned server version |
| Xapian needs system libs (GeoDjango-class problem) | docker image for CI; loud-skip locally with install hint |
| IR can't express an engine feature (e.g. ES `_all` fuzziness, Solr stats) | IR ops are additive; backend declares unsupported-with-warning; gap table in docs is generated from the IR |
| Two packages drifting (`django_pitchfork` vs `pitchfork`) | One-way import rule (no back-edges); CI enforces via import-linter in the replacement repo; the engine repo has no knowledge of the replacement |
| Engine and replacement release out of step (`django-pitchfork` needs a feature the pinned engine lacks) | `django-pitchfork` declares a `pitchfork-engine>=X` floor it was tested against; bumping is an explicit, reviewed change, never an open range |
| AltParser/`raw_search` bypass the IR | Allowed by design; each backend documents its raw syntax; conformance suite keeps raw tests per backend |
| Silent test skips (haystack's known problem) | Runner prints a loud skip summary and fails CI if an owned service (pitchfork fixture, Whoosh/Xapian) is unavailable |
