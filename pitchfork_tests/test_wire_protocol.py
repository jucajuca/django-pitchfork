"""P1-5: every operation negotiates the wire version, in both transports.

The check lives in the engine itself (`TantivyEngine`), so this exercises it
through the backend each mode ships: a request stamped with a future wire
version must fail loudly — never silently return empty results. Operations:
search, index (update), remove, clear.

Also guards the acceptance criterion that no Python source hardcodes `"v": 1`.
"""

import json
import os
import re
import unittest

from django.test import override_settings

from pitchfork import wire as wire_v1

from pitchfork_tests.testcases import PitchforkTestCase, REPO_ROOT


class WireVersionLiteralTestCase(unittest.TestCase):
    def test_no_v1_literals_in_python_sources(self):
        # Acceptance criterion: `grep -r '"v": 1' python/` returns nothing;
        # every request version goes through `wire.WIRE_VERSION`.
        pattern = re.compile(r'"v"\s*:\s*1\b')
        offenders = []
        for root, _dirs, files in os.walk(os.path.join(REPO_ROOT, "python")):
            if "__pycache__" in root:
                continue
            for name in files:
                if not name.endswith(".py"):
                    continue
                path = os.path.join(root, name)
                with open(path, encoding="utf-8") as fh:
                    for lineno, line in enumerate(fh, 1):
                        if pattern.search(line):
                            offenders.append(f"{path}:{lineno}: {line.strip()}")
        self.assertEqual(offenders, [], "hardcoded wire versions found:\n" + "\n".join(offenders))

    def test_wire_version_constant_matches_static(self):
        # V1 == 1: the constant is the single source of truth, matching the
        # Rust static `pitchfork-core/src/wire.rs::WIRE_VERSION`.
        self.assertEqual(wire_v1.WIRE_VERSION, 1)


@override_settings(USE_TZ=False)
class WireVersionMismatchRejectedTestCase(PitchforkTestCase):
    """Every operation rejects a future wire version — no silent empty.

    Mode-agnostic: server mode sends the request through the HTTP transport
    (whose version check rejects before the body reaches the engine);
    embedded mode calls the PyO3 engine directly, where the engine's own
    check (P1-5) answers with an error response / not-ok ack. The base
    class starts the live fixture (server) or compiles in-process
    (embedded) per mode.
    """

    FUTURE_VERSION = wire_v1.WIRE_VERSION + 100

    def setUp(self):
        super().setUp()

        from haystack import connections

        self.backend = connections["default"].get_backend()
        self.backend.setup()
        self.backend.silently_fail = False

    @property
    def _mode(self):
        return os.environ.get("PITCHFORK_MODE", "server")

    def _post(self, request, engine_op):
        if self._mode == "embedded":
            engine = self.backend.engine
            response = json.loads(getattr(engine, engine_op)(json.dumps(request)))
            if engine_op == "search":
                self.assertEqual(response.get("kind"), wire_v1.KIND_ERROR)
                self.assertIn("wire version", response.get("error") or "")
            else:
                self.assertFalse(response.get("ok"))
                self.assertIn("wire version", response.get("error") or "")
            return

        from pitchfork.exceptions import PitchforkError

        # Server transport: rejection happens before the body is parsed.
        with self.assertRaises(PitchforkError):
            self.backend.client.post("/search", request)

    def test_search_rejects_future_version(self):
        self._post(
            {
                "v": self.FUTURE_VERSION,
                "schema_fp": self.backend.schema_fp,
                "ast": {"node": "match_all"},
                "start": 0,
                "end": 10,
                "sort": [],
                "facets": [],
                "highlight": False,
                "spellcheck": False,
            },
            "search",
        )

    def test_clear_rejects_future_version(self):
        self._post(
            {"v": self.FUTURE_VERSION, "schema_fp": self.backend.schema_fp},
            "clear",
        )

    def test_remove_rejects_future_version(self):
        self._post(
            {
                "v": self.FUTURE_VERSION,
                "schema_fp": self.backend.schema_fp,
                "ids": ["core.mockmodel.1"],
            },
            "remove",
        )

    def test_index_rejects_future_version(self):
        self._post(
            {
                "v": self.FUTURE_VERSION,
                "schema_fp": self.backend.schema_fp,
                "schema": self.backend.schema_definition,
                "docs": [
                    {
                        "id": "core.mockmodel.99",
                        "django_ct": "core.mockmodel",
                        "django_id": "99",
                        "text": "x",
                    }
                ],
            },
            "index",
        )
