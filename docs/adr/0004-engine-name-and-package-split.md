# The engine is `pitchfork`; the replacement API is `django-pitchfork`

This repo's package is named **`pitchfork`** (import) / **`pitchfork-engine`**
(distribution). The haystack-replacement API — `SearchQuerySet`, template
tags, forms, views, management commands — will ship as a **separate** package,
**`django-pitchfork`**, which depends on `pitchfork`.

```
pitchfork            this repo: Rust/Tantivy engine, wire codec, haystack backends
      ▲
django-pitchfork     future: the haystack-replacement API, built on pitchfork
```

## Context

ADR 0002 assumed the opposite split: `django-pitchfork` = the released native
engine, `pitchfork` = the not-yet-built replacement. That naming was never
shipped — the repo was renamed before its first release (no git tags; the
names are unclaimed on PyPI and crates.io), so the dichotomy cost nothing to
invert.

Two facts forced the final naming:

1. **The imported package must not be called `django-something`.** It is not a
   Django app: it has no `AppConfig`, no models, no migrations, and it is
   imported by configuration (`ENGINE = "…"`), not added to `INSTALLED_APPS`.
   The `django-` prefix is a promise of framework integration this layer does
   not make.
2. **PyPI's `pitchfork` is occupied** by an unrelated project (v0.1.5,
   2018-08-20, an API client for pitchfork.com reviews; abandoned, last
   supports Python 3.6). The import name and the distribution name therefore
   cannot match. `pitchfork-engine` is free and states what the package is.

## Considered Options

- **Keep ADR 0002's split (engine = `django-pitchfork`)**: rejected — the
  engine is the part that changes least and has no Django dependency; naming
  it after Django misleads and blocks use from non-Django callers.
- **Invert the arrow (API = `pitchfork`, engine = `pitchfork.engine`)**: kept
  as the *structural* model — the volatile Django layer sits on top — but not
  as a single distribution. See the naming decision below.
- **One distribution containing both layers**: rejected — the replacement API
  has a different release cadence, different optional dependencies (whoosh,
  pysolr, elasticsearch, xapian), and a different audience.
- **Publish as `pitchfork` and wait for the squatter**: rejected — blocks the
  rename on a third party; not a technical blocker we can clear.
- **PEP 541 abandoned-project request for `pitchfork`**: deferred, not
  rejected. It would let the import and distribution names match. Tracked as
  future work; nothing in this ADR depends on the outcome.

## Consequences

- Usage: `pip install pitchfork-engine` then `import pitchfork`. The README
  and `docs/MIGRATION.md` state this pairing in their first paragraph, because
  it is the one non-obvious thing about installing the package.
- `pitchfork._core` stays a **private** extension module. `pitchfork.engine` is
  reserved in the glossary for a possible public engine facade; adding one is
  not required and does not gate the replacement API.
- The wire protocol, `pitchfork-server` binary, and the `pitchfork-core` /
  `pitchfork-engine` / `pitchfork-server` crates need no change: they were
  already named after the engine.
- ADR 0002's constraint "ENGINE paths must stay stable for existing adopters"
  is void — nothing had been published under the old paths. The old engine
  module path `django_pitchfork.backends.*` is not aliased.
- The replacement API's base-class dependency (which ADR 0002 addressed) must
  be re-decided when that package starts: it will consume
  `pitchfork.backends.base` rather than duplicating haystack's
  `BaseSearchQuery` machinery.
