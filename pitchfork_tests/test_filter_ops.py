"""Table-driven filter-op x field-type coverage (P0-3, P0-4, P2-1).

Every haystack lookup (`exact`, `contains`, `startswith`, `endswith`,
`gt/gte/lt/lte`, `in`, `range`, `fuzzy`, and the bare lookup that encodes as
`content`) is asserted for the field types the mock index covers.

Uses a dedicated index whose prepared values actually vary per document (the
shared ``test_backend`` mock index uses ``default=`` fields that never read
the model).
"""

import datetime

from django.test.utils import override_settings
from haystack import connections, indexes
from haystack.query import SearchQuerySet
from haystack.utils.loading import UnifiedIndex

from test_haystack.core.models import MockModel
from pitchfork_tests.testcases import PitchforkTestCase


class FilterOpsIndex(indexes.SearchIndex, indexes.Indexable):
    text = indexes.CharField(document=True)
    name = indexes.CharField(model_attr="author")
    pub_date = indexes.DateTimeField(model_attr="pub_date")
    views = indexes.IntegerField()
    rating = indexes.FloatField()
    is_active = indexes.BooleanField()

    def get_model(self):
        return MockModel

    def prepare_text(self, obj):
        return "Indexed!\n%s" % obj.pk

    def prepare_views(self, obj):
        return obj.views

    def prepare_rating(self, obj):
        return float(obj.views) / 10.0

    def prepare_is_active(self, obj):
        return bool(obj.views % 2)


def mock(id, name, pub_date, views):
    m = MockModel()
    m.id = id
    m.author = name
    m.pub_date = pub_date
    m.views = views
    return m


@override_settings(USE_TZ=False)
class FilterOpsTableTestCase(PitchforkTestCase):
    def setUp(self):
        super().setUp()

        self.old_ui = connections["default"].get_unified_index()
        self.ui = UnifiedIndex()
        self.mock_index = FilterOpsIndex()
        self.ui.build(indexes=[self.mock_index])
        self.sb = connections["default"].get_backend()
        connections["default"]._index = self.ui

        self.sb.setup()
        self.sb.clear()

        self.sqs = SearchQuerySet("default")

        self.sb.update(
            self.mock_index,
            [
                mock(1, "daniel", datetime.datetime(2020, 1, 1, 12, 30, 0), 5),
                mock(2, "david", datetime.datetime(2021, 6, 15, 8, 0, 0), 20),
                mock(3, "anita", datetime.datetime(2022, 12, 31, 23, 59, 59), 50),
            ],
        )

    # --- exact / bare lookups on typed fields (P0-3) -------------------

    def test_bare_integer_lookup_matches(self):
        self.assertEqual(self.sqs.filter(views=20).count(), 1)
        self.assertEqual(self.sqs.filter(views=20)[0].name, "david")

    def test_bare_integer_equals_exact(self):
        self.assertEqual(
            self.sqs.filter(views=20).count(),
            self.sqs.filter(views__exact=20).count(),
        )

    def test_bare_boolean_lookup_matches(self):
        # views 5, 20, 50 -> is_active True, False, False.
        self.assertEqual(self.sqs.filter(is_active=False).count(), 2)
        self.assertEqual(self.sqs.filter(is_active=True).count(), 1)
        self.assertEqual(self.sqs.filter(is_active=True)[0].name, "daniel")

    def test_bare_datetime_lookup_matches(self):
        self.assertEqual(
            self.sqs.filter(pub_date=datetime.datetime(2021, 6, 15, 8, 0, 0)).count(), 1
        )

    def test_bare_float_lookup_matches(self):
        # views 5, 20, 50 -> rating 0.5, 2.0, 5.0.
        self.assertEqual(self.sqs.filter(rating=5.0).count(), 1)
        self.assertEqual(self.sqs.filter(rating=5.0)[0].name, "anita")
        self.assertEqual(self.sqs.filter(rating=2.0).count(), 1)
        self.assertEqual(self.sqs.filter(rating=2.0)[0].name, "david")

    # --- contains semantics (P0-4) -------------------------------------

    def test_contains_is_substring_on_char(self):
        # "ani" sits inside BOTH "daniel" and "anita" — substring, not
        # prefix/exact (P0-4).
        self.assertEqual(self.sqs.filter(name__contains="ani").count(), 2)
        # A needle unique to one value:
        self.assertEqual(self.sqs.filter(name__contains="tan").count(), 0)
        self.assertEqual(self.sqs.filter(name__contains="iel").count(), 1)
        self.assertEqual(self.sqs.filter(name__contains="iel")[0].name, "daniel")

    def test_contains_on_analyzed_text(self):
        self.assertEqual(self.sqs.filter(text__contains="Indexed").count(), 3)

    def test_contains_regex_metachars(self):
        # Values with regex metacharacters must not be interpreted as
        # regex fragments.
        self.assertEqual(self.sqs.filter(name__contains="a.(").count(), 0)

    def test_startswith_endswith(self):
        self.assertEqual(self.sqs.filter(name__startswith="an").count(), 1)
        self.assertEqual(self.sqs.filter(name__startswith="da").count(), 2)
        self.assertEqual(self.sqs.filter(name__endswith="d").count(), 1)
        self.assertEqual(self.sqs.filter(name__endswith="d")[0].name, "david")

    # --- numeric comparisons --------------------------------------------

    def test_gt_gte_lt_lte(self):
        self.assertEqual(self.sqs.filter(views__gt=5).count(), 2)
        self.assertEqual(self.sqs.filter(views__gte=20).count(), 2)
        self.assertEqual(self.sqs.filter(views__lt=20).count(), 1)
        self.assertEqual(self.sqs.filter(views__lte=20).count(), 2)

    # --- dates (P2-1) ----------------------------------------------------

    def test_in_with_string_dates(self):
        self.assertEqual(
            self.sqs.filter(pub_date__in=["2020-01-01", "2022-12-31"]).count(), 2
        )

    def test_in_with_date_values_widens_to_day(self):
        # P2-1: haystack normalizes `date` values through `_convert_datetime`;
        # a date-only entry must match any time within that calendar day.
        self.assertEqual(
            self.sqs.filter(pub_date__in=[datetime.date(2022, 12, 31)]).count(), 1
        )

    def test_in_with_datetime_values(self):
        self.assertEqual(
            self.sqs.filter(
                pub_date__in=[datetime.datetime(2021, 6, 15, 8, 0, 0)]
            ).count(),
            1,
        )

    def test_range_on_datetime(self):
        self.assertEqual(
            self.sqs.filter(
                pub_date__range=[
                    datetime.datetime(2020, 1, 1),
                    datetime.datetime(2021, 12, 31),
                ]
            ).count(),
            2,
        )

    # --- fuzzy -----------------------------------------------------------

    def test_fuzzy(self):
        self.assertEqual(self.sqs.filter(name__fuzzy="danile").count(), 1)
        self.assertEqual(self.sqs.filter(name__fuzzy="danile")[0].name, "daniel")
