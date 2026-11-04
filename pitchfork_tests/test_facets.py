"""P0-5: facet counts must be page-independent, and reaching the
``facet_scan_limit`` must be reported via a `truncated` marker.
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.query import SearchQuerySet
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.testcases import PitchforkTestCase


class FacetIndex(indexes.SearchIndex, indexes.Indexable):
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
        return [self.tag_word(obj.pk)]

    @staticmethod
    def tag_word(pk):
        return "alpha" if pk % 2 == 0 else "beta"


def mock(id, name, pub_date):
    m = MockModel()
    m.id = id
    m.author = name
    m.pub_date = pub_date
    return m


@override_settings(USE_TZ=False)
class FacetPageIndependenceTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = FacetIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        self.sqs = SearchQuerySet("default")

        pub_dates = [datetime.datetime(2020, 1, 1 + day) for day in range(6)]
        docs = [mock(pk + 1, f"author{pk}", pub_dates[pk]) for pk in range(6)]
        self.sb.update(self.mock_index, docs)

    def _window(self, start, end):
        """Clone with an explicit result window (start/end offset)."""
        clone = self.sqs.all()
        if start is not None:
            clone.query.start_offset = start
        if end is not None:
            clone.query.end_offset = end
        return clone

    def _tag_counts(self, sqs):
        counts = sqs.facet("tags").facet_counts()
        return dict(counts["fields"]["tags"])

    def test_counts_identical_across_window_sizes(self):
        full = self._tag_counts(self.sqs.all())
        for start, end in [(-1, None), (0, 1), (0, 2), (2, 4), (0, 6), (0, 10)]:
            if start == -1:
                start = None  # parametrize the no-start case too
            window = self._tag_counts(self._window(start, end))
            self.assertEqual(
                window,
                full,
                "facet counts changed with window (%s, %s)" % (start, end),
            )
        self.assertEqual(full.get("alpha"), 3)
        self.assertEqual(full.get("beta"), 3)

    def test_counts_invariant_to_order_by(self):
        default = self._tag_counts(self.sqs.all())
        ordered = self._tag_counts(self.sqs.order_by("-pub_date"))
        self.assertEqual(ordered, default)

    def test_truncated_flag_concrete(self):
        # Every match fits inside the shipped scan limit: no truncation.
        counts = self.sqs.facet("tags").facet_counts()
        self.assertIsNone(counts.get("truncated"))
