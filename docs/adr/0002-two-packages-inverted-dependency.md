# Two packages with an inverted dependency arrow

The repo ships two packages: **django-pitchfork** (released: native Rust
engine bindings + wire codec) and **pitchfork** (the haystack-replacement
API). As released, `django_pitchfork` subclasses haystack's
`BaseEngine`/`BaseSearchBackend`/`BaseSearchQuery` — it cannot be imported
without haystack installed. Since pitchfork is a *replacement*, the base
classes move to `pitchfork.backends.base` and `django_pitchfork` re-points
at them, inverting the arrow. Haystack stays a dev dependency only
(conformance test apps + the compat-shim parity test).

## Considered Options

- **pitchfork imports django_pitchfork, django_pitchfork keeps haystack
  deps**: rejected — a "replacement" that transitively requires haystack
  defeats the goal (grill finding F1).
- **Vendor a copy of the base classes into django_pitchfork**: rejected —
  two copies of the query machinery drift.
- **One package**: rejected — django-pitchfork is released under its name;
  ENGINE paths must stay stable for existing adopters.

## Consequences

- Import direction is strictly: `pitchfork.backends.base` ←
  `django_pitchfork` ← `pitchfork.backends.*` (and pitchfork's other
  modules). No runtime haystack imports anywhere.
- The inversion is a phase-1 deliverable with a test: the native backend
  passes the conformance subset with haystack uninstalled.
