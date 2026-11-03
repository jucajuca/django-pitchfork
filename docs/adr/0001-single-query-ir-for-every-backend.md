# One query IR for every backend

Every backend lowers queries from a single engine-neutral query IR
(pitchfork-core's QueryNode AST) instead of building engine-native query
strings/DSLs directly from `SearchQuery` state.

haystack's backends each re-implement ~20 of the same methods (query
building, schema building, result processing) against their engine's DSL
with no shared representation — ~5,200 duplicated lines across Solr, ES
(×3 version-split files), and Whoosh. Filter-op parity bugs land six times.
The replacement centralizes: one IR, one `build_query_fragment`, per-backend
*lowering* modules only.

## Considered Options

- **haystack's status quo** (per-backend query builders): rejected — the
  duplication is the maintenance hole this replacement exists to close.
- **Per-backend IRs**: rejected — same drift, more code.

## Consequences

- Engine features not expressible in the IR are declared
  unsupported-with-warning by the backend; the docs backend-support table is
  generated from the IR (BackendCaps), so the table cannot drift.
- Filter-op fixes and property tests live at the IR, not in six copies.
- `raw_search`/`AltParser` deliberately bypass the IR (backend-native
  syntax); each backend documents its raw form.
