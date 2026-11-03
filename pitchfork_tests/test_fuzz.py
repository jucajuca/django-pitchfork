"""Property-based fuzz tests (Hypothesis) on the wire codec and AST encoder.

Discipline (bolt's rule): parsers get property tests on both sides —
`proptest!` next to the Rust parser, Hypothesis here. CI runs fixed cases;
the weekly fuzz workflow runs extended random ones.
"""

import json

from hypothesis import given, settings, strategies as st

from django_pitchfork import wire as wire_v1
from django_pitchfork.backends import base as adapter_base


@st.composite
def wire_values(draw):
    return draw(
        st.one_of(
            st.none(),
            st.booleans(),
            st.integers(min_value=-(2**62), max_value=2**62),
            st.floats(allow_nan=False, allow_infinity=False, width=64),
            st.text(max_size=64),
            st.lists(st.text(max_size=16), max_size=4),
        )
    )


class TestWireCodecFuzz:
    @given(value=wire_values())
    @settings(max_examples=200, deadline=None)
    def test_encode_wire_value_round_trips_json(self, value):
        encoded = adapter_base.encode_wire_value(value)
        # Every wire value must survive a JSON round trip unchanged.
        assert json.loads(json.dumps(encoded)) == encoded

    @given(
        expression=st.one_of(
            st.just("title"),
            st.from_regex(r"[a-z]+(__[a-z]+){0,2}", fullmatch=True),
        )
    )
    @settings(max_examples=200, deadline=None)
    def test_split_expression_always_yields_valid_op(self, expression):
        field, filter_type = wire_v1.split_expression(expression)
        assert field
        assert filter_type in wire_v1.FILTER_OPS

    @given(
        field=st.from_regex(r"[a-z]{1,8}", fullmatch=True),
        lookup=st.sampled_from(sorted(wire_v1.FILTER_OPS)),
        value=wire_values(),
    )
    @settings(max_examples=200, deadline=None)
    def test_encode_filter_produces_json_safe_nodes(self, field, lookup, value):
        node = adapter_base.encode_filter(field, lookup, value)
        assert node["node"] == "filter"
        assert node["op"] in wire_v1.FILTER_OPS
        # JSON-safe.
        json.dumps(node)

    @given(
        points=st.lists(
            st.tuples(
                st.floats(min_value=-90, max_value=90, allow_nan=False),
                st.floats(min_value=-180, max_value=180, allow_nan=False),
            ),
            min_size=1,
            max_size=3,
        )
    )
    @settings(max_examples=100, deadline=None)
    def test_encode_point_handles_geojson_tuples(self, points):
        for lat, lng in points:
            encoded = wire_v1.encode_point((lng, lat))
            parts = encoded.split(",")
            assert len(parts) == 2
            assert float(parts[0]) == lat
            assert float(parts[1]) == lng
