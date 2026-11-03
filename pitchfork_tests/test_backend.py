"""Backend round-trip tests against a live pitchfork-server fixture.

Mirrors the structure of haystack's whoosh_tests: real backend, real server,
no mocks for what we own.
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes, reset_search_queries
from haystack.models import SearchResult
from haystack.query import SQ, SearchQuerySet
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.testcases import PitchforkTestCase


class PitchforkMockSearchIndex(indexes.SearchIndex, indexes.Indexable):
    text = indexes.CharField(document=True)
    name = indexes.CharField(model_attr="author")
    pub_date = indexes.DateTimeField(model_attr="pub_date")
    views = indexes.IntegerField(default=0)
    is_active = indexes.BooleanField(default=True)
    tags = indexes.MultiValueField()

    def get_model(self):
        return MockModel

    def prepare_text(self, obj):
        return "Indexed!\n%s" % obj.pk

    def prepare_tags(self, obj):
        return ["a", "b"]


@override_settings(USE_TZ=False)
class LivePitchforkRoundTripTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        # Stow.
        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = PitchforkMockSearchIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        self.sqs = SearchQuerySet("default")

        # Fake indexing.
        mock = MockModel()
        mock.id = 1
        mock.author = "daniel"
        mock.pub_date = datetime.datetime(2020, 1, 1, 12, 30, 0)
        self.sb.update(self.mock_index, [mock])

        mock2 = MockModel()
        mock2.id = 2
        mock2.author = "david"
        mock2.pub_date = datetime.datetime(2021, 6, 15, 8, 0, 0)
        self.sb.update(self.mock_index, [mock2])

    def tearDown(self):
        super().tearDown()

    def test_round_trip(self):
        results = self.sqs.filter(id="core.mockmodel.1")

        # Sanity check.
        self.assertEqual(results.count(), 1)

        # Check the individual fields.
        result = results[0]
        self.assertEqual(result.id, "core.mockmodel.1")
        self.assertEqual(result.text, "Indexed!\n1")
        self.assertEqual(result.name, "daniel")

    def test_filter_exact(self):
        self.assertEqual(
            self.sqs.filter(name="daniel").count(),
            1,
        )
        self.assertEqual(
            self.sqs.filter(name__exact="david").count(),
            1,
        )
        self.assertEqual(self.sqs.filter(name__exact="nobody").count(), 0)

    def test_filter_contains(self):
        results = self.sqs.filter(text__contains="Indexed")
        self.assertEqual(results.count(), 2)

    def test_filter_startswith(self):
        results = self.sqs.filter(name__startswith="da")
        self.assertEqual(results.count(), 2)
        results = self.sqs.filter(name__startswith="dav")
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "david")

    def test_filter_numeric_comparisons(self):
        self.assertEqual(self.sqs.filter(views__gt=0).count(), 0)
        results = self.sqs.filter(pub_date__gt="2021-01-01")
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "david")

    def test_filter_in(self):
        results = self.sqs.filter(name__in=["daniel", "clay"])
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")

    def test_filter_range(self):
        results = self.sqs.filter(pub_date__range=["2020-01-01", "2021-12-31"])
        self.assertEqual(results.count(), 2)

    def test_filter_fuzzy(self):
        # Distance 2: "danile" matches "daniel" (1 insert) but not "david".
        results = self.sqs.filter(name__fuzzy="danile")
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")

    def test_exclude(self):
        self.assertEqual(self.sqs.exclude(name="daniel").count(), 1)
        self.assertEqual(self.sqs.exclude(name="daniel")[0].name, "david")

    def test_or_composition(self):
        results = self.sqs.filter(SQ(name="daniel") | SQ(name="david"))
        self.assertEqual(results.count(), 2)

    def test_not_composition(self):
        results = self.sqs.exclude(SQ(name="daniel") | SQ(name="david"))
        self.assertEqual(results.count(), 0)
        results = self.sqs.filter(~SQ(name="daniel"))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "david")

    def test_order_by(self):
        results = self.sqs.order_by("-pub_date")
        self.assertEqual(results.count(), 2)
        self.assertEqual(results[0].name, "david")
        self.assertEqual(results[1].name, "daniel")

    def test_pagination(self):
        results = self.sqs.order_by("pub_date")
        page = results[0:1]
        self.assertEqual(len(page), 1)
        self.assertEqual(page[0].name, "daniel")

    def test_remove(self):
        self.sb.remove(MockModel(id=1))
        self.assertEqual(self.sqs.count(), 1)
        self.assertEqual(self.sqs[0].name, "david")

    def test_clear(self):
        self.sb.clear()
        self.assertEqual(self.sqs.count(), 0)

    def test_facets(self):
        results = self.sqs.facet("name")
        counts = results.facet_counts()
        self.assertIn("fields", counts)
        names = dict(counts["fields"]["name"])
        self.assertEqual(names["daniel"], 1)
        self.assertEqual(names["david"], 1)

    def test_more_like_this(self):
        mock = MockModel(id=1, author="daniel")
        results = self.sqs.more_like_this(mock)
        # The source document must not be included.
        self.assertTrue(all(result.id != "core.mockmodel.1" for result in results))

    def test_result_class(self):
        class CustomSearchResult(SearchResult):
            pass

        results = self.sqs.result_class(CustomSearchResult).all()
        self.assertTrue(results)
        self.assertIsInstance(results[0], CustomSearchResult)
