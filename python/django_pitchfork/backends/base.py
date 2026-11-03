"""Shared adapter logic: SQ-tree -> wire AST encoding, value coercion, HTTP.

The adapter compiles config once at registration (`setup`) — never per query
(bolt's "do it once at registration" rule). Query objects are encoded into
the PitchforkWireV1 AST here and decoded from results back into
``SearchQuerySet``-compatible results in ``backends/server.py``.
"""

import datetime
import json
import logging
import urllib.error
import urllib.request
from decimal import Decimal

from django.conf import settings
from haystack.constants import (
    DJANGO_CT,
    DJANGO_ID,
    ID,
    ITERATOR_LOAD_PER_QUERY,
    DOCUMENT_FIELD,
)
from haystack.exceptions import SearchBackendError
from haystack.inputs import AutoQuery, Clean, Exact, Raw
from haystack.utils import get_model_ct

from ..exceptions import PitchforkServerError, PitchforkWireError
from .. import wire as wire_v1
from ..wire import encode_point, spatial_dwithin, spatial_filter, spatial_within  # noqa: F401

# Spatial helpers are part of the wire module (both-sides rule); re-exported
# here so adapters can use them via the shared base module.

# Default limit when scanning all matches (mirrors the engine's window cap).
FACET_SCAN_LIMIT = 10_000


class PitchforkClient:
    """Minimal stdlib HTTP client speaking PitchforkWireV1."""

    def __init__(self, base_url, timeout=10):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _headers(self):
        return {
            "Content-Type": "application/json",
            wire_v1.WIRE_HEADER: str(wire_v1.WIRE_VERSION),
        }

    def post(self, path, payload, expected="auto"):
        body = json.dumps(payload).encode("utf-8")
        request = urllib.request.Request(
            self.base_url + path, data=body, headers=self._headers(), method="POST"
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                data = json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as exc:
            detail = exc.read().decode("utf-8", errors="replace")
            raise self._http_error(path, exc, detail) from exc
        except urllib.error.URLError as exc:
            raise self._http_error(path, exc, "") from exc

        self._check_version(data)
        if expected == "ack" and data.get("ok") is not True:
            raise PitchforkServerError(
                f"pitchfork-server {path} failed: {data.get('error')}"
            )
        return data

    def _http_error(self, path, exc, detail):
        return PitchforkServerError(
            f"pitchfork-server {self.base_url}{path} failed: {exc} {detail}"
        )

    @staticmethod
    def _check_version(data):
        if data.get("v") != wire_v1.WIRE_VERSION:
            raise PitchforkWireError(
                f"wire version mismatch: server sent {data.get('v')}, "
                f"adapter speaks {wire_v1.WIRE_VERSION}"
            )


def encode_wire_value(value):
    """Coerce a Python value into a PitchforkWireV1 JSON value.

    Datetimes travel as ISO-8601 strings (the engine parses them back per
    field type); Decimals as strings (haystack coerces decimals to strings).
    """
    if isinstance(value, datetime.datetime):
        return value.isoformat()
    if isinstance(value, datetime.date):
        return value.isoformat()
    if isinstance(value, datetime.time):
        return value.isoformat()
    if isinstance(value, Decimal):
        return str(value)
    if isinstance(value, (list, tuple)):
        return [encode_wire_value(item) for item in value]
    if isinstance(value, (bool, int, float, str)) or value is None:
        return value
    # Last resort: stringify (haystack's backends do the same for exotic types).
    return str(value)


def _input_value(value):
    """Unwrap haystack input types into plain wire values."""
    if isinstance(value, (AutoQuery, Clean, Exact, Raw)):
        return value.value
    return value


def encode_filter(field, filter_type, value):
    """Encode a single (field, filter_type, value) leaf into a filter node."""
    value = _input_value(value)

    # AutoQuery is a full-text content lookup regardless of the field name.
    if isinstance(value, AutoQuery) or filter_type == "content":
        op = "content"
        cleaned = value
        if isinstance(cleaned, (Clean, Exact, Raw)):
            cleaned = cleaned.value
        return {
            "node": "filter",
            "field": field,
            "op": op,
            "value": str(cleaned) if cleaned is not None else None,
        }

    return {
        "node": "filter",
        "field": field,
        "op": filter_type,
        "value": encode_wire_value(value),
    }


def build_ast(search_node):
    """Encode haystack's ``SearchNode`` SQ tree into a PitchforkWireV1 AST.

    SearchNode children entries are either nested SearchNodes or
    ``(expression, value)`` pairs where expression is ``field`` or
    ``field__lookup``.
    """
    children = []
    for child in search_node.children:
        if hasattr(child, "as_query_string"):
            children.append(build_ast(child))
        else:
            expression, value = child
            field, filter_type = split_expression_wire(expression)
            children.append(encode_filter(field, filter_type, value))

    if not children:
        return {"node": "match_all"}

    if len(children) == 1:
        node = children[0]
    elif search_node.connector == "OR":
        node = {"node": "or", "children": children}
    else:
        node = {"node": "and", "children": children}

    if search_node.negated:
        return {"node": "not", "child": node}
    return node


def split_expression_wire(expression):
    """Wire-side split (kept separate so tests can exercise both modules)."""
    return wire_v1.split_expression(expression)


def build_schema_definition(searchfields):
    """Compile haystack SearchField classes into a PitchforkWireV1 schema.

    This is the registration-time compile: the resulting definition travels
    to the server once, which fingerprints it and hard-errors on later
    mismatch — never a silent rebuild.
    """
    fields = []
    content_field_name = ""

    for _, field_class in sorted(searchfields.items()):
        field_type = field_type_wire(field_class)
        fields.append(
            {
                "name": field_class.index_fieldname,
                "type": field_type,
                "stored": bool(field_class.stored),
                "indexed": bool(field_class.indexed),
                "multi": bool(field_class.is_multivalued),
                "document": bool(field_class.document),
                "boost": float(field_class.boost),
            }
        )
        if field_class.document is True:
            content_field_name = field_class.index_fieldname

    return content_field_name or DOCUMENT_FIELD, {"fields": fields}


def field_type_wire(field_class):
    """Map a haystack SearchField to a wire FieldType name."""
    if field_class.document:
        # The document field is analyzed (full-text).
        return "text"
    if field_class.is_multivalued:
        return "char"
    return {
        "string": "char",
        "date": "date",
        "datetime": "date_time",
        "integer": "integer",
        "float": "float",
        "boolean": "boolean",
        "location": "location",
        "ngram": "ngram",
        "edge_ngram": "edge_ngram",
    }.get(field_class.field_type, "char")


def models_filter(models):
    """OR-of-django_ct-exact narrow filter for the given models."""
    return [
        {"node": "filter", "field": DJANGO_CT, "op": "exact", "value": get_model_ct(model)}
        for model in models
    ]


def limit_to_registered_models():
    return getattr(settings, "HAYSTACK_LIMIT_TO_REGISTERED_MODELS", True)


def iterator_load_per_query():
    return ITERATOR_LOAD_PER_QUERY
