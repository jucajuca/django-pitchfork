# One Elasticsearch backend for 7/8+

Support Elasticsearch with a single backend (elasticsearch-py 7.17–8.x)
instead of haystack's three-file version split (`elasticsearch_backend.py`
for 1.x–2.x, `elasticsearch5_backend.py` for 5–6, `elasticsearch7_backend.py`
for 7–8) — haystack's own AGENTS.md flags the split as a mirroring liability:
behavior changes must be applied three times.

## Considered Options

- **Match haystack's version split**: rejected — three copies of the query
  lowering to maintain; the whole point of the IR is one lowering.
- **Support 1.x–2.x too**: rejected — servers that old are past EOL and the
  client libs no longer test against them.

## Consequences

- If the 8.x client's compat mode with 7.17 servers proves painful, the
  documented floor is lowered to 8.x servers only — the split is not
  reintroduced.
- CI pins the compose service version; the destructive-teardown guard
  (refuse non-test index prefixes) carries over from haystack's tests.
