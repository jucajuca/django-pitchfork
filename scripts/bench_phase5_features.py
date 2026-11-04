"""Phase 5 feature-cost benchmark: each feature on top of a full-text query.

Run: .venv/bin/python scripts/bench_phase5_features.py
Numbers go into BENCHMARK.md (bolt's lesson: measure, write the numbers down).
"""

import json
import os
import sys
import time
from tempfile import mkdtemp

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python"))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "django-haystack"))

from pitchfork import _core  # noqa: E402

N = 100

SCHEMA = {
    "fields": [
        {"name": "text", "type": "text", "stored": True, "indexed": True, "multi": False, "document": True, "boost": 1.0},
        {"name": "name", "type": "char", "stored": True, "indexed": True, "multi": False, "document": False, "boost": 1.0},
        {"name": "location", "type": "location", "stored": True, "indexed": True, "multi": False, "document": False, "boost": 1.0},
    ]
}


def mkdocs(n):
    docs = []
    for i in range(n):
        docs.append(
            {
                "id": f"core.mockmodel.{i}",
                "django_ct": "core.mockmodel",
                "django_id": str(i),
                "name": f"author{i % 10}",
                "text": f"indexed! document number {i} with words alpha beta gamma",
                "location": f"{40.0 + (i % 100) / 100.0},{-10.0 + (i % 50) / 10.0}",
            }
        )
    return docs


def request(**overrides):
    req = {
        "v": 1,
        "schema_fp": FP,
        "ast": {"node": "filter", "field": "name", "op": "exact", "value": "author5"},
        "start": 0,
        "end": 20,
        "sort": [],
        "facets": [],
        "highlight": False,
        "spellcheck": False,
        "spelling_query": None,
        "spatial": None,
    }
    req.update(overrides)
    return json.dumps(req)


def bench(label, fn):
    start = time.perf_counter()
    for _ in range(N):
        fn()
    elapsed = time.perf_counter() - start
    print(f"| {label:<32} | {elapsed * 1000:.0f} ms | {elapsed / N * 1000:.3f} ms |")


engine = _core.PyPitchforkEngine(mkdtemp())
engine.setup(json.dumps(SCHEMA))
engine.index(json.dumps({"v": 1, "schema_fp": None, "schema": SCHEMA, "docs": mkdocs(500)}))
FP = engine.schema_fp()

# Baseline full-text.
bench("Baseline full-text", lambda: engine.search(request()))

# + facets (1 field).
bench("+ facets (1 field)", lambda: engine.search(request(facets=["name"])))

# + highlight snippets.
bench("+ highlight snippets", lambda: engine.search(request(highlight=True)))

# + spelling suggestion (typo'd query, dictionary hit).
bench("+ spelling suggestion", lambda: engine.search(request(spellcheck=True, spelling_query="indxed! document")))

# + within (bounding box).
bench("+ within (bounding box)", lambda: engine.search(request(
    ast={"node": "match_all"},
    spatial={"kind": "within", "field": "location", "point_1": "40.0,-10.0", "point_2": "41.0,-5.0"},
)))

# + dwithin (haversine postfilter).
bench("+ dwithin (haversine postfilter)", lambda: engine.search(request(
    ast={"node": "match_all"},
    spatial={"kind": "dwithin", "field": "location", "point": "40.5,-7.5", "radius_km": 300.0},
)))
