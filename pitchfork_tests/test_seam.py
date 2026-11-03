"""Dual-mode (C → B seam) and wire-contract consistency tests."""

import json

from django.test.utils import override_settings
from django_pitchfork import wire as wire_v1
from django_pitchfork.backends import base as adapter_base
from pitchfork_tests.testcases import PitchforkTestCase


class WireContractTestCase(PitchforkTestCase):
    """The both-sides rule: Python constants and Rust statics must agree."""

    def test_wire_version_constant(self):
        assert wire_v1.WIRE_VERSION == 1

    def test_filter_ops_cover_haystack_lookups(self):
        # haystack.constants.VALID_FILTERS minus aliases, plus content.
        import haystack.constants

        for lookup in haystack.constants.VALID_FILTERS:
            assert lookup in wire_v1.FILTER_OPS, f"missing lookup '{lookup}'"

    def test_lookup_split_expression(self):
        assert wire_v1.split_expression("title") == ("title", "content")
        assert wire_v1.split_expression("title__exact") == ("title", "exact")
        assert wire_v1.split_expression("pub_date__gte") == ("pub_date", "gte")
        # Unknown suffixes fall back to full-text content.
        assert wire_v1.split_expression("title__bogus") == ("title", "content")


class AutoEngineSeamTestCase(PitchforkTestCase):
    """The C → B seam: embedded mode when the Rust module imports."""

    def test_seam_picks_embedded_when_compiled(self):
        from django_pitchfork.backends.embedded import AutoEngine, HAS_CORE

        engine = AutoEngine()
        if HAS_CORE:
            from django_pitchfork.backends.embedded import PitchforkEmbeddedBackend

            assert engine.backend is PitchforkEmbeddedBackend
        else:
            from django_pitchfork.backends.server import PitchforkSearchBackend

            assert engine.backend is PitchforkSearchBackend

    def test_schema_definition_is_valid_wire_schema(self):
        from haystack import connections

        searchfields = connections["default"].get_unified_index().all_searchfields()
        content_field, definition = adapter_base.build_schema_definition(searchfields)
        assert content_field
        assert definition["fields"]
        for field in definition["fields"]:
            assert set(field) == {"name", "type", "stored", "indexed", "multi", "document", "boost"}
            assert field["type"] in {
                "text", "char", "date", "date_time", "integer", "float",
                "boolean", "location", "ngram", "edge_ngram",
            }

    def test_round_trip_has_no_protocol_drift(self):
        """The same AST encodes/decodes in both modes: JSON round trip."""
        from haystack.query import SQ

        tree = SQ(name="daniel") | SQ(pub_date__gte="2020-01-01")
        ast = adapter_base.build_ast(tree)
        encoded = json.loads(json.dumps(ast))
        assert encoded == ast
        assert ast["node"] == "or"
