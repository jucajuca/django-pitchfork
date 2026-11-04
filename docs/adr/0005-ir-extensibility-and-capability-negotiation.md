# IR extensibility: additive ops, wire-declared capabilities

New per-engine features are added to the shared query IR as **additive nodes**,
and each backend **declares** which IR ops it supports as part of the wire
handshake. A backend that does not support a declared op fails loudly; it never
silently no-ops.

This ADR fixes the mechanism, because ADR 0001's "IR ops are additive" and
"unsupported features are declared-with-warning" were principles without a
concrete contract. Without one, "extensible IR" is a docs claim, and the
absence of a feature is discoverable only by trying it and getting wrong
results.

## Context

ADR 0001 chose one engine-neutral `QueryNode` AST in `pitchfork-core`, lowered
per engine. Every backend that follows haystack must express the same query
surface, while each engine has features the others lack (Solr `stats`, ES
`_all` fuzziness, MLT term parameters, distance annotation, date/query facets).

Two failure modes had to be designed out:

1. **Silent no-ops.** Today the native engine accepts `boost`,
   `distance_point`, `stats`, `date_facet` and `query_facet` and ignores them.
   A caller gets plausible-looking results with no signal that a control was
   dropped — the worst outcome for a search API, because it looks like a
   ranking bug rather than an unsupported feature.
2. **Protocol churn.** If every added feature bumped `WIRE_VERSION`, each
   small capability would need a coordinated engine + client + replacement
   release, which in practice means features get delayed or bundled.

The wire already has the property that makes this tractable: `QueryNode` is
serde-tagged (`#[serde(tag = "node")]`), and request structs use
`#[serde(default)]`. An unknown node tag is a deserialization error ("unknown
variant"), not silent acceptance; a new optional request field is invisible to
an old client.

## Decision

1. **Additive IR nodes do not bump `WIRE_VERSION`.** A new `QueryNode` variant
   or `FilterOp` value is a backward-compatible addition: new clients emit it,
   old engines reject it loudly, old clients never send it. `WIRE_VERSION` is
   reserved for *semantic* changes to existing ops (for example redefining
   `contains` from equality to substring) and for new **required** request
   fields.
2. **Capabilities travel on the wire.** `/setup`'s acknowledgement includes a
   `caps` block — the supported IR ops and coarse feature flags (highlight,
   facet modes, spatial, spelling). The client may therefore negotiate, and the
   conformance suite and generated docs table both read the same declaration.
3. **Unsupported means loud.** A backend receiving an op it has not declared
   must warn (Python-side `warnings.warn`, engine-side logged warning carried in
   the response) or return an explicit error. Dropping a control silently is a
   defect, not a gap.
4. **Provisional features use an extension envelope.** Features not yet ready
   to be IR citizens travel in `"pitchfork_extensions": {…}` on the request,
   which engines that do not understand it ignore. Promoting one to a real IR
   node is then a small, testable change rather than a protocol negotiation.
5. **Each new op ships with two tests**: behaviour where `caps` says supported,
   and the loud-warning path where it does not.

## Considered Options

- **Docs table as the source of truth**: rejected — it drifts, it is not
  machine-readable at runtime, and it cannot fail a test.
- **Bump `WIRE_VERSION` for every addition**: rejected — forces lockstep
  releases for changes that are backward-compatible by construction, and
  encourages batching unrelated features.
- **Per-backend IRs**: rejected in ADR 0001 (same drift, more code).
- **Silent no-op for unsupported ops** (status quo): rejected — indistinguishable
  from a ranking bug; it is exactly the class of drift this project exists to
  remove.
- **Hard error for every unsupported op**: rejected as the *only* behaviour —
  haystack itself continues with a warning for several of these (e.g. Whoosh
  query faceting), and a replacement that raises where haystack warned would
  break migrations. Hence "warn by default, error where haystack errored".

## Consequences

- Adding a feature is a four-step pattern (IR node, caps declaration,
  per-engine lowering, capability-gated tests) with no changes to existing ops
  — that is the extensibility property the replacement plan relies on.
- The backend-support documentation table is **generated** from `caps`
  declarations, so it cannot claim support that the code does not have.
- `caps` becomes a compatibility surface of its own: adding a flag is additive,
  removing or renaming one is a breaking change and follows the same rules as
  the wire itself.
- The native engine must publish honest caps, which means its current
  accepted-and-ignored features (`boost`, `distance_point`, date/query facets,
  `stats`) must be reported as **unsupported** until they are implemented —
  converting silent gaps into loud ones. That work is enumerated as P1-1/P1-2
  in `pitchfork-engine-defect-fixes.md`.
- Semantic changes to existing ops still need a coordinated release; the
  `contains` fix (P0-4) is the first such change and ships with a changelog
  note naming the behaviour difference.
