"""PitchforkWireV1 — the Python side of the versioned wire protocol.

Design rules (from the django-bolt lessons):
- One versioned contract; every new cross-boundary type adds this Python
  constant and the Rust static (`pitchfork-core/src/wire.rs::WIRE_VERSION`)
  in the same commit.
- Results use int body kinds (0 = hits, 1 = empty, 2 = error).
"""

# Wire protocol version. Must match pitchfork-core/src/wire.rs::WIRE_VERSION.
WIRE_VERSION = 1

# Response body kinds.
KIND_HITS = 0
KIND_EMPTY = 1
KIND_ERROR = 2

# Wire header name (must match pitchfork-server/src/main.rs::WIRE_HEADER).
WIRE_HEADER = "X-Pitchfork-Wire"

# Filter lookups (mirror of haystack.constants.VALID_FILTERS and the
# snake_case serde names in pitchfork-core/src/query_ast.rs::FilterOp).
FILTER_OPS = frozenset(
    {
        "exact",
        "contains",
        "startswith",
        "endswith",
        "gt",
        "gte",
        "lt",
        "lte",
        "in",
        "range",
        "fuzzy",
        "content",
    }
)

# Operation tags (request side).
OP_SEARCH = 0
OP_INDEX = 1
OP_REMOVE = 2
OP_CLEAR = 3

# Geospatial filter kinds (mirror of pitchfork-core/src/wire.rs
# SpatialFilterV1's serde tags; changed in the same commit).
SPATIAL_WITHIN = "within"
SPATIAL_DWITHIN = "dwithin"


def encode_point(point):
    """Coerce a haystack point into the wire's "lat,lng" string format.

    Handles GEOS points (``.coords`` -> (lng, lat)), GeoJSON-alike
    tuples/lists (lng, lat), dicts ({lat, lon}), and existing
    "lat,lng" strings — mirroring haystack's LocationField.convert.
    """
    if point is None:
        return None
    if isinstance(point, str):
        # Assume already "lat,lng".
        return point
    if hasattr(point, "coords"):
        lng, lat = point.coords
        return f"{lat},{lng}"
    if isinstance(point, dict):
        lat = point.get("lat", 0)
        lng = point.get("lon", 0)
        return f"{lat},{lng}"
    if isinstance(point, (list, tuple)):
        # GeoJSON-alike: (lng, lat).
        lat, lng = point[1], point[0]
        return f"{lat},{lng}"
    raise TypeError(f"Unable to extract coordinates from {point!r}")


def spatial_within(field, point_1, point_2):
    """haystack `within` (bounding box) -> SpatialFilterV1."""
    return {
        "kind": SPATIAL_WITHIN,
        "field": field,
        "point_1": encode_point(point_1),
        "point_2": encode_point(point_2),
    }


def spatial_dwithin(field, point, radius_km):
    """haystack `dwithin` (radius, km) -> SpatialFilterV1."""
    return {
        "kind": SPATIAL_DWITHIN,
        "field": field,
        "point": encode_point(point),
        "radius_km": float(radius_km),
    }


def spatial_filter(within=None, dwithin=None):
    """Build the spatial filter from haystack's within/dwithin kwargs.

    haystack passes ``within = {"field", "point_1", "point_2"}`` and
    ``dwithin = {"field", "point", "distance"}`` (distance has ``.km``).
    V1 supports one spatial filter per search (first entry wins).
    """
    if within:
        return spatial_within(
            within["field"], within["point_1"], within["point_2"]
        )
    if dwithin:
        radius_km = getattr(dwithin["distance"], "km", dwithin["distance"])
        return spatial_dwithin(dwithin["field"], dwithin["point"], radius_km)
    return None

FILTER_SEPARATOR = "__"


def split_expression(expression):
    """Parses an expression and determines the field and filter type.

    Mirror of ``haystack.backends.BaseSearchQuery.split_expression``: unknown
    `__` suffixes fall back to the full-text ``content`` lookup.
    """
    parts = expression.split(FILTER_SEPARATOR)
    field = parts[0]
    if len(parts) == 1 or parts[-1] not in FILTER_OPS:
        filter_type = "content"
    else:
        filter_type = parts.pop()
    return field, filter_type
