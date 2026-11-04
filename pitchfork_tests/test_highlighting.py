"""P0-6: `highlight()` must return snippets containing the matched term.

``result.highlighted`` used to always be empty because the engine's snippet
map was keyed by field name while the join looked up by document id.
"""

import datetime

from django.test.utils import override_settings
from haystack import connections
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
class HighlightingTestCase(PitchforkTestCase):
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

    def test_highlight_returns_fragments(self):
        results = self.sqs.auto_query("indexed").highlight()
        self.assertEqual(results.count(), 2)
        for result in results:
            self.assertIsNotNone(result.highlighted)
            fragments = list(result.highlighted.values())[0]
            self.assertTrue(
                any("Indexed" in fragment or "indexed" in fragment for fragment in fragments),
                "fragment must contain the matched term: %r" % (fragments,),
            )

    def test_highlight_fragment_covers_document_field(self):
        results = self.sqs.filter(content="indexed").highlight()
        for result in results:
            highlighted = result.highlighted or {}
            self.assertIn("text", highlighted, "snippets keyed by the document field")
