"""Spelling suggestion tests (ported from haystack's whoosh spelling tests)."""

import json

from django.test.utils import override_settings
from haystack import connections
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.test_backend import PitchforkMockSearchIndex
from pitchfork_tests.testcases import PitchforkTestCase


@override_settings(USE_TZ=False)
class PitchforkSpellingTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        # Stow and swap in the mock index.
        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = PitchforkMockSearchIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        mock = MockModel()
        mock.id = 1
        mock.author = "daniel"
        mock.pub_date = __import__("datetime").datetime(2020, 1, 1)
        self.sb.update(self.mock_index, [mock])

    def tearDown(self):
        super().tearDown()

    def test_spelling_suggestion(self):
        # "indxed! daniel" misspells a dictionary term; the backend suggests
        # the correction. Suggested words are alphanumeric (whoosh's
        # suggestion is also a token join).
        results = self.sb.search(
            json.dumps({"node": "match_all"}),
            spelling_query="indxed! daniel",
        )
        self.assertEqual(results["spelling_suggestion"], "indexed daniel")

    def test_no_suggestion_for_clean_query(self):
        results = self.sb.search(
            json.dumps({"node": "match_all"}),
            spelling_query="indexed! daniel",
        )
        self.assertIsNone(results["spelling_suggestion"])

    def test_spelling_via_search_query_set(self):
        from haystack.query import SearchQuerySet

        # set_spelling_query returns a clone; use it.
        self.sqs = SearchQuerySet("default").set_spelling_query("indxed! daniel")
        self.assertEqual(self.sqs.spelling_suggestion(), "indexed daniel")
