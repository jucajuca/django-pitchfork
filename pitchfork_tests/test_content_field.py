"""P0-2 regression tests: the haystack ``content`` pseudo-field maps to the
index's document field (whoosh: ``index_fieldname = ""``).

``filter(content=...)``, ``auto_query(...)``, and ``raw_search(...)`` must
return real documents instead of failing with "unknown field 'content'".
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.inputs import Raw
from haystack.query import SearchQuerySet
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.test_backend import PitchforkMockSearchIndex
from pitchfork_tests.testcases import PitchforkTestCase


def mock(id, author, pub_date):
    m = MockModel()
    m.id = id
    m.author = author
    m.pub_date = pub_date
    return m


@override_settings(USE_TZ=False)
class ContentFieldTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = PitchforkMockSearchIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        self.sqs = SearchQuerySet("default")

        self.sb.update(
            self.mock_index,
            [
                mock(1, "daniel", datetime.datetime(2020, 1, 1, 12, 30, 0)),
                mock(2, "david", datetime.datetime(2021, 6, 15, 8, 0, 0)),
            ],
        )

    def test_filter_content_field(self):
        results = self.sqs.filter(content="indexed")
        self.assertEqual(results.count(), 2)

    def test_auto_query_hits_document_field(self):
        results = self.sqs.auto_query("indexed")
        self.assertEqual(results.count(), 2)

    def test_raw_search(self):
        # raw_search lowers to a content op on the document field, so the
        # term must live in the indexed text ("Indexed! N").
        results = self.sqs.raw_search("indexed")
        self.assertEqual(results.count(), 2)
        # An author-only term does not hit the document field.
        self.assertEqual(self.sqs.raw_search("daniel").count(), 0)

    def test_raw_search_with_raw_input(self):
        results = self.sqs.filter(content=Raw("indexed"))
        self.assertEqual(results.count(), 2)

    def test_content_and_field_filter_combination(self):
        results = self.sqs.filter(content="indexed").filter(name="daniel")
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")
