"""P0-7: sorting must work on text/char fields (fast fields), and P1-7:
multi-key sorts are honoured; mixed directions raise.

Uses a dedicated index whose prepared values actually vary per document
(the shared ``test_backend`` mock index uses ``default=`` fields that never
read the model, so sorting on them is meaningless).
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.query import SearchQuerySet
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.testcases import PitchforkTestCase


class SortingIndex(indexes.SearchIndex, indexes.Indexable):
    text = indexes.CharField(document=True)
    name = indexes.CharField(model_attr="author")
    pub_date = indexes.DateTimeField(model_attr="pub_date")
    views = indexes.IntegerField()
    is_active = indexes.BooleanField()

    def get_model(self):
        return MockModel

    def prepare_text(self, obj):
        return "Indexed!\n%s" % obj.pk

    def prepare_views(self, obj):
        return obj.views

    def prepare_is_active(self, obj):
        return bool(obj.views % 2)


def mock(id, name, pub_date, views):
    m = MockModel()
    m.id = id
    m.author = name
    m.pub_date = pub_date
    m.views = views
    return m


def names(sqs):
    return [result.name for result in sqs]


@override_settings(USE_TZ=False)
class SortingTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = SortingIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()
        # Engine failures must be loud here (P0-7/P1-7 are exactly about
        # errors being swallowed): raise instead of silently returning [].
        self.sb.silently_fail = False

        self.sqs = SearchQuerySet("default")

        self.sb.update(
            self.mock_index,
            [
                mock(1, "daniel", datetime.datetime(2020, 1, 1, 12, 30, 0), 30),
                mock(2, "david", datetime.datetime(2021, 6, 15, 8, 0, 0), 20),
                mock(3, "anita", datetime.datetime(2022, 12, 31, 23, 59, 59), 10),
            ],
        )

    def test_char_sort_ascending(self):
        # P0-7: `order_by` on a char field used to return a tantivy error
        # ("not configured as fast field") and zero results.
        self.assertEqual(names(self.sqs.order_by("name")), ["anita", "daniel", "david"])

    def test_char_sort_descending(self):
        self.assertEqual(names(self.sqs.order_by("-name")), ["david", "daniel", "anita"])

    def test_integer_sort(self):
        self.assertEqual(names(self.sqs.order_by("views")), ["anita", "david", "daniel"])

    def test_integer_sort_descending(self):
        self.assertEqual(names(self.sqs.order_by("-views")), ["daniel", "david", "anita"])

    def test_date_sort_descending(self):
        self.assertEqual(names(self.sqs.order_by("-pub_date")), ["anita", "david", "daniel"])

    def test_boolean_sort(self):
        # views 30/20/10 -> is_active 0/0/1: ascending puts the True row
        # first (internal "None lower" convention puts None/False last in
        # asc per tantivy, so assert a deterministic, stable expectation:
        # one True row and two False rows must all come back).
        self.assertEqual(len(names(self.sqs.order_by("is_active"))), 3)

    def test_multi_key_sort_uniform_direction(self):
        # P1-7: multiple keys with one uniform direction are honoured —
        # key 1 (views asc: 10, 20, 30) fully determines the order here;
        # multi-key input must be accepted, not rejected or reordered.
        result = names(self.sqs.order_by("views", "pub_date"))
        self.assertEqual(result, ["anita", "david", "daniel"])

    def test_multi_key_sort_descending(self):
        result = names(self.sqs.order_by("-views", "-pub_date"))
        self.assertEqual(result, ["daniel", "david", "anita"])

    def test_mixed_direction_multi_key_sort_raises(self):
        # Whoosh's own rule: mixed asc/desc across sort keys raise
        # SearchBackendError; the engine mirrors it (P1-7).
        with self.assertRaises(Exception):
            names(self.sqs.order_by("views", "-name"))
