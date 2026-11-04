# One query IR for every backend

Every backend lowers queries from a single engine-neutral query IR
(pitchfork-core's QueryNode AST) instead of building engine-native query
strings/DSLs directly from `SearchQuery` state.

haystack's backends each re-implement ~20 of the same methods (query
building, schema building, result processing) against their engine's DSL
with no shared representation — 4,263 lines across Solr (977), ES (1,129,
plus 475 for `es5` and 578 for `es7`), and Whoosh (1,104). Filter-op parity
bugs land six times. The replacement centralizes: one IR, one
`build_query_fragment`, per-backend *lowering* modules only.

## Considered Options

- **haystack's status quo** (per-backend query builders): rejected — the
  duplication is the maintenance hole this replacement exists to close.
- **Per-backend IRs**: rejected — same drift, more code.

## Consequences

- Engine features not expressible in the IR are declared
  unsupported-with-warning by the backend. Declarations travel on the wire:
  `/setup`'s acknowledgement carries a `caps` block (supported IR ops,
  highlight/facet modes), so the client and the conformance suite negotiate
  instead of guessing, and the docs backend-support table is generated from the
  same declarations so it cannot drift.
- **Extensibility contract.** New per-engine features are added as additive IR
  nodes: a new `QueryNode`/`FilterOp` variant is backward-compatible because
  nodes are serde-tagged and request fields are `#[serde(default)]` — an old
  engine rejects an unknown tag loudly, an old client simply never sends it.
  `WIRE_VERSION` is bumped only for semantic changes to existing ops or new
  *required* fields, never for additive ones. Backends that do not implement a
  newly declared op must warn, never no-op silently.
- Filter-op fixes and property tests live at the IR, not in six copies. This
  concentrates blast radius but does not by itself guarantee correctness: the
  single site must be *right*. The native engine currently lowers
  `FilterOp::Contains` identically to `Exact`, i.e. the centralized site has
  exactly the drift this ADR sets out to prevent — see the P0 list in
  `pitchfork-engine-defect-fixes.md`. Centralization makes such a bug a
  one-line fix; it does not prevent it, so IR lowering needs per-op
  conformance tests against haystack semantics.
- `raw_search`/`AltParser` deliberately bypass the IR (backend-native
  syntax); each backend documents its raw form.
