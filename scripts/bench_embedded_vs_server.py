"""Rough embedded-vs-server benchmark: N queries through each wire mode.

Run: .venv/bin/python scripts/bench_embedded_vs_server.py
Numbers go into BENCHMARK.md (bolt's lesson: measure, write the numbers down).
"""

import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "python"))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), "django-haystack"))
os.environ["DJANGO_SETTINGS_MODULE"] = "pitchfork_tests.settings"

import django

django.setup()

from pitchfork.backends.base import PitchforkClient, build_schema_definition  # noqa: E402
from pitchfork_tests.testcases import PitchforkServerFixture  # noqa: E402
from pitchfork import _core  # noqa: E402

N = 300

SCHEMA = {
    "fields": [
        {"name": "text", "type": "text", "stored": True, "indexed": True, "multi": False, "document": True, "boost": 1.0},
        {"name": "name", "type": "char", "stored": True, "indexed": True, "multi": False, "document": False, "boost": 1.0},
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
            }
        )
    return docs


def bench(label, fn):
    start = time.perf_counter()
    fn()
    elapsed = time.perf_counter() - start
    print(f"{label}: {elapsed * 1000:.0f} ms total, {elapsed / N * 1000:.3f} ms/query")
    return elapsed


# --- Embedded ---
def run_embedded():
    from tempfile import mkdtemp

    engine = _core.PyPitchforkEngine(mkdtemp())
    engine.setup(json.dumps(SCHEMA))
    engine.index(json.dumps({"v": 1, "schema_fp": None, "schema": SCHEMA, "docs": mkdocs(500)}))
    req = json.dumps(
        {
            "v": 1,
            "schema_fp": engine.schema_fp(),
            "ast": {"node": "filter", "field": "name", "op": "exact", "value": "author5"},
            "start": 0,
            "end": 20,
            "sort": [],
            "facets": [],
            "highlight": False,
            "spellcheck": False,
        }
    )
    for _ in range(N):
        resp = json.loads(engine.search(req))
        assert resp["total"] == 50, resp["total"]


# --- Server ---
def run_server():
    fixture = PitchforkServerFixture().start()
    client = PitchforkClient(fixture.url)
    ack = client.post("/setup", {"v": 1, "schema_fp": None, "schema": SCHEMA, "docs": []}, expected="ack")
    fp = ack["schema_fp"]
    client.post("/index", {"v": 1, "schema_fp": fp, "schema": SCHEMA, "docs": mkdocs(500)}, expected="ack")
    req = {
        "v": 1,
        "schema_fp": fp,
        "ast": {"node": "filter", "field": "name", "op": "exact", "value": "author5"},
        "start": 0,
        "end": 20,
        "sort": [],
        "facets": [],
        "highlight": False,
        "spellcheck": False,
    }
    for _ in range(N):
        resp = client.post("/search", req)
        assert resp["total"] == 50, resp["total"]
    fixture.stop()


print(f"running {N} queries per mode (500 docs, exact char filter)...")
e = bench("embedded", run_embedded)
s = bench("server (HTTP)", run_server)
print(f"embedded vs server speedup: {s / e:.2f}x")
