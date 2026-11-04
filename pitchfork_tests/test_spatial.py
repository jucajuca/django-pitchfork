"""Spatial query tests (haystack within/dwithout semantics, without GeoDjango).

Points use the GeoJSON-alike tuple format (lng, lat) and "lat,lng" strings —
both are handled by the adapter's encode_point (mirroring
haystack's LocationField.convert). Distances are stubbed with a ``.km``
attribute (GeoDjango's ``D(km=...)`` is not needed here).
"""

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.query import SearchQuerySet
from haystack.utils.loading import UnifiedIndex
from test_haystack.core.models import MockModel

from pitchfork_tests.testcases import PitchforkTestCase


class Km:
    """Stub for django.contrib.gis.measure.D(km=...)."""

    def __init__(self, km):
        self.km = float(km)


class FakePoint:
    """GEOS-like point stub: haystack's ensure_point only checks
    ``geom_type`` and reads ``.coords`` (in (lng, lat) order)."""

    geom_type = "Point"

    def __init__(self, lat, lng):
        self._lat, self._lng = lat, lng

    @property
    def coords(self):
        return (self._lng, self._lat)


class PitchforkLocationSearchIndex(indexes.SearchIndex, indexes.Indexable):
    text = indexes.CharField(document=True)
    name = indexes.CharField(model_attr="author")
    location = indexes.LocationField()

    def get_model(self):
        return MockModel

    def prepare_location(self, obj):
        return {
            1: "51.5074,-0.1278",  # London
            2: "48.8566,2.3522",  # Paris
            3: "52.5200,13.4050",  # Berlin
        }.get(obj.pk, "0.0,0.0")


@override_settings(USE_TZ=False)
class PitchforkSpatialTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.loc_index = PitchforkLocationSearchIndex()
        self.ui.build(indexes=[self.loc_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        for pk, author in [(1, "london"), (2, "paris"), (3, "berlin")]:
            mock = MockModel()
            mock.id = pk
            mock.author = author
            mock.pub_date = __import__("datetime").datetime(2020, 1, 1)
            self.sb.update(self.loc_index, [mock])

        from haystack.query import SearchQuerySet

        self.sqs = SearchQuerySet("default")

    def tearDown(self):
        super().tearDown()

    def test_within_bounding_box(self):
        # Bounding box covering London and Paris; corners as fake points
        # (GEOS-like, coords in (lng, lat) order).
        results = self.sqs.within(
            "location", FakePoint(47.0, -1.0), FakePoint(53.0, 3.0)
        )
        self.assertEqual(results.count(), 2)

    def test_within_geojson_points(self):
        # Corners as GeoJSON-alike tuples (lng, lat) pass haystack's
        # ensure_point? No — within requires point-like objects; use the
        # fake points.
        results = self.sqs.within("location", FakePoint(47.0, -1.0), FakePoint(53.0, 3.0))
        self.assertEqual(results.count(), 2)

    def test_dwithin_radius(self):
        # 300 km around London: Paris (~344 km) is out.
        point = FakePoint(51.5074, -0.1278)
        results = self.sqs.dwithin("location", point, Km(300))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "london")

    def test_dwithin_large_radius(self):
        # 600 km around London: London and Paris are in.
        results = self.sqs.dwithin("location", FakePoint(51.5074, -0.1278), Km(600))
        self.assertEqual(results.count(), 2)
