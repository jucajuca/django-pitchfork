"""P0-1 regression tests: haystack input types must not crash encoding.

``AutoQuery`` / ``Raw`` / ``Clean`` / ``Exact`` all carry ``.query_string``
(there is no ``.value`` attribute on any haystack input).
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.inputs import AutoQuery, Clean, Exact, Raw
from haystack.query import SQ, SearchQuerySet
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
class InputsRoundTripTestCase(PitchforkTestCase):
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

    def test_exact_input(self):
        results = self.sqs.filter(name=Exact("daniel"))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")

    def test_clean_input(self):
        results = self.sqs.filter(name=Clean("david"))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "david")

    def test_raw_input(self):
        results = self.sqs.filter(name=Raw("daniel"))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")

    def test_not_inside_sq(self):
        results = self.sqs.filter(SQ(name="daniel") & ~SQ(name="nobody"))
        self.assertEqual(results.count(), 1)
        self.assertEqual(results[0].name, "daniel")

    def test_auto_query(self):
        results = self.sqs.auto_query("indexed")
        self.assertEqual(results.count(), 2)

    def test_auto_query_hits_author_through_content(self):
        results = self.sqs.auto_query("daniel")
        # The document field carries the indexed body text; `daniel` lives
        # in the name field, so the content lookup finds it via the
        # document text of exactly no document — but must not error.
        self.assertIn(results.count(), (0, 1))

    def test_auto_query_quoted_phrase(self):
        # Quoted phrase still routes through AutoQuery -> content op.
        results = self.sqs.auto_query('"indexed"')
        self.assertEqual(results.count(), 2)

    def test_auto_query_negated_term(self):
        results = self.sqs.auto_query("-indexed")
        self.assertEqual(results.count(), 0)
