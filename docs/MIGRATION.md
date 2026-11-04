# Migrating to PitchforkEngine

This guide covers switching an existing django-haystack project from a
classic backend (Whoosh, Solr, Elasticsearch) to `PitchforkEngine`.

## 1. Install

```sh
pip install pitchfork-engine
```

The distribution is `pitchfork-engine`; the import package is `pitchfork`.
(`pitchfork` on PyPI is an unrelated, abandoned 2018 project.) This package is
the **engine**; the haystack-replacement API ships separately as
`django-pitchfork`.

The package ships version-specific wheels (one per supported CPython —
see `.github/workflows/release.yml`); there is no compiler requirement at
install time.

## 2. Point the connection at Pitchfork

In `settings.py`, swap the `ENGINE` of each haystack connection:

```python
HAYSTACK_CONNECTIONS = {
    "default": {
        "ENGINE": "pitchfork.backends.embedded.AutoEngine",
        "PATH": os.path.join(BASE_DIR, "search-index"),
    },
}

HAYSTACK_SIGNAL_PROCESSOR = (
    "haystack.signals.RealtimeSignalProcessor"  # unchanged
)
```

Embedded (in-process Rust) vs server (separate pitchfork-server process) is
selected automatically by `AutoEngine`: if the compiled
`pitchfork._core` extension imports, the embedded engine is used;
otherwise the adapter talks HTTP to a running `pitchfork-server`. To pin a
mode explicitly:

```python
# Force server mode (Strategy C):
"ENGINE": "pitchfork.backends.server.PitchforkEngine",
# Force embedded mode (Strategy B):
"ENGINE": "pitchfork.backends.embedded.PitchforkEmbeddedEngine",
```

Server mode needs the `pitchfork-server` binary running and reachable
(`connection_options["URL"]`, default `http://127.0.0.1:8117`).

## 3. Rebuild the index

```sh
python manage.py rebuild_index
```

Pitchfork compiles the index schema from your `SearchIndex` classes the same
way the other backends do; no schema file to port. The compiled schema is
fingerprinted — a stale server (schema fingerprint mismatch) is a hard error,
never a silent rebuild.

## 4. What carries over unchanged

Everything in the haystack query API:

- `SearchQuerySet` chains: `filter`, `exclude`, `auto_query`, `order_by`,
  `facet`, `highlight`, `more_like_this`, `within`, `dwithin`.
- `SearchForm` / `ModelSearchForm` and views.
- Signal processors, management commands, `Index` classes.

Result objects (`SearchResult`) expose the same fields; `highlight`ing and
faceting are computed by the Rust core.

## 5. Per-backend notes

### From WhooshBackend

- Spelling suggestions work the same way: set `INCLUDE_SPELLING: True` on the
  connection (or use `sqs.set_spelling_query(...)`); the suggestion is the
  closest dictionary term per word (Levenshtein distance ≤ 2, ranked by
  document frequency).
- `within`/`dwithin` use the same haystack `D(km=...)` distances and point
  formats.

### From SolrBackend / ElasticsearchBackend

- There is no external service to configure in embedded mode; in server mode
  `pitchfork-server` replaces the Solr/Elasticsearch daemon.
- Haystack's alt-parser input (`Raw` / `AltParser`) is not supported; use
  haystack's standard `AutoQuery`/`Clean` inputs.
- Approximation-only behavior of the remote engines (e.g. Elasticsearch's
  `_all` fuzziness) is replaced by exact Tantivy semantics: bare-field and
  content filters match exact terms on non-document fields and tokenized
  full text on document fields.

## 6. Compatibility contract

The Python adapter and the Rust core talk **PitchforkWireV1** — one
versioned wire protocol:

- Every cross-boundary type exists as a Python constant
  (`pitchfork/wire.py`) and a Rust static
  (`pitchfork-core/src/wire.rs`), changed in the same commit.
- Wire changes bump `WIRE_VERSION`; the adapter negotiates before the first
  request and fails loudly on a mismatch.
- A schema fingerprint is computed at `/setup` (or engine setup) and sent
  with every request; mismatch = hard error.
