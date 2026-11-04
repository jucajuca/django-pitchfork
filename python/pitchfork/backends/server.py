"""Strategy C: unmodified django-haystack works over HTTP to pitchfork-server.

The adapter subclasses ``haystack.backends.BaseEngine`` exactly like the
Whoosh/Solr/ES backends do; switching
``HAYSTACK_CONNECTIONS["default"]["ENGINE"]`` from WhooshEngine to
``PitchforkEngine`` requires no other change.
"""

import json
import logging
import warnings

from haystack.backends import BaseEngine, BaseSearchBackend, BaseSearchQuery, log_query
from haystack.constants import DJANGO_CT, DJANGO_ID
from haystack.exceptions import SkipDocument
from haystack.models import SearchResult
from haystack.utils import get_identifier, get_model_ct
from haystack.utils.app_loading import haystack_get_model

from ..exceptions import PitchforkError
from .. import wire as wire_v1
from . import base as adapter_base
from .base import PitchforkClient, build_ast, encode_wire_value


class ResultProcessingMixin:
    """Shared hit->result processing for both wire modes (server + embedded).

    Both modes decode identical PitchforkWireV1 responses, so this mixin
    exercises the same protocol contract and it cannot drift."""

    # P2-2: these names are positional or reserved kwargs on haystack's
    # SearchResult constructor; a stored field with one of these names must
    # not be splatted through `**additional_fields`.
    RESERVED_RESULT_FIELDS = frozenset(
        {"score", "app_label", "model_name", "pk", "_distance", "_point_of_origin"}
    )

    def _decode_query(self, query_string):
        """Our build_query() emits JSON ASTs; raw query strings (raw_search)
        fall back to a full-text content lookup."""
        try:
            decoded = json.loads(query_string)
        except (ValueError, TypeError):
            decoded = None
        if isinstance(decoded, dict) and "node" in decoded:
            return self._map_content_fields(decoded)
        return {
            "node": "filter",
            "field": self.content_field_name,
            "op": "content",
            "value": str(query_string),
        }

    def _map_content_fields(self, node):
        """P0-2: map the haystack ``content`` pseudo-field onto the index's
        document field, recursively over AND/OR/NOT nodes (whoosh maps
        ``content`` to ``index_fieldname = ""``/"no special field")."""
        if node.get("node") == "filter" and node.get("field") in ("content", ""):
            mapped = dict(node)
            mapped["field"] = self.content_field_name
            return mapped
        if "children" in node:
            mapped = dict(node)
            mapped["children"] = [self._map_content_fields(c) for c in node["children"]]
            return mapped
        if "child" in node:
            mapped = dict(node)
            mapped["child"] = self._map_content_fields(node["child"])
            return mapped
        return node

    def _resolve_model_and_index(self, app_label, model_name):
        """P1-6 seam: resolve a wire ``django_ct`` pair to ``(model, index)``.

        Injectable so the engine can run query-time results processing
        without haystack (replacement-plan Phase 1); the default resolves
        through haystack's UnifiedIndex. Returns ``(model, index)`` with
        either possibly ``None``.
        """
        from haystack import connections
        from haystack.utils.app_loading import haystack_get_model

        unified_index = connections[self.connection_alias].get_unified_index()
        model = haystack_get_model(app_label, model_name)
        if not model:
            return None, None
        if model not in unified_index.get_indexed_models():
            return model, None
        return model, unified_index.get_index(model)

    def _process_results(self, response, highlight=False, result_class=None):
        if result_class is None:
            result_class = SearchResult

        results = []
        hits = response.get("total", 0)

        facets = {"fields": {}, "dates": {}, "queries": {}}
        for field_name, pairs in (response.get("facets") or {}).items():
            facets["fields"][field_name] = [
                (value, count) for (value, count) in pairs
            ]
        # P0-5: the engine flags facet counts that only cover a prefix of
        # the match set (facet_scan_limit reached).
        if response.get("facets_truncated"):
            facets["truncated"] = True

        for hit in response.get("hits", []):
            app_label, model_name = hit["django_ct"].split(".")
            score = hit.get("score") or 0
            model, index = self._resolve_model_and_index(app_label, model_name)

            if index is not None:
                # SearchResult.id comes from the stored identifier field
                # (whoosh passes it through the same way).
                additional_fields = {"id": hit["id"]}
                for key, value in hit.get("stored", {}).items():
                    # P2-2: haystack's SearchResult reserves these kwargs;
                    # splatting a stored field named `score` would raise
                    # `TypeError ... got multiple values`. Fail with a
                    # warning instead, like a loud backend should.
                    if key in self.RESERVED_RESULT_FIELDS:
                        self.log.warning(
                            "Stored field '%s' collides with a reserved "
                            "SearchResult name; value dropped from result "
                            "(%s.%s)" % (key, app_label, model_name),
                        )
                        continue
                    if key in index.fields and hasattr(index.fields[key], "convert"):
                        if index.fields[key].is_multivalued:
                            additional_fields[key] = list(value or [])
                        else:
                            additional_fields[key] = index.fields[key].convert(value)
                    else:
                        additional_fields[key] = self._to_python(value)
            else:
                self.log.warning(
                    "No index for model '%s.%s'; results returned without "
                    "field conversion" % (app_label, model_name),
                )
                additional_fields = {"id": hit["id"]}
                for key, value in hit.get("stored", {}).items():
                    if key in self.RESERVED_RESULT_FIELDS:
                        continue
                    additional_fields[key] = self._to_python(value)

            if highlight and hit.get("snippets"):
                additional_fields["highlighted"] = {
                    field: list(fragments)
                    for field, fragments in hit["snippets"].items()
                }

            result = result_class(
                app_label,
                model_name,
                hit["django_id"],
                score,
                **additional_fields,
            )
            results.append(result)

        spelling_suggestion = response.get("spelling")

        return {
            "results": results,
            "hits": hits,
            "facets": facets,
            "spelling_suggestion": spelling_suggestion,
        }

    @staticmethod
    def _to_python(value):
        import datetime
        import re

        if value is None:
            return None

        if isinstance(value, str):
            datetime_regex = re.compile(
                r"^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})"
                r"(T|\s+)(?P<hour>\d{2}):(?P<minute>\d{2}):(?P<second>\d{2})"
            )
            date_regex = re.compile(r"^(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})$")

            match = datetime_regex.search(value)
            if match:
                data = match.groupdict()
                return datetime.datetime(
                    int(data["year"]),
                    int(data["month"]),
                    int(data["day"]),
                    int(data["hour"]),
                    int(data["minute"]),
                    int(data["second"]),
                )
            match = date_regex.search(value)
            if match:
                data = match.groupdict()
                return datetime.date(
                    int(data["year"]), int(data["month"]), int(data["day"])
                )
        return value


class PitchforkSearchBackend(ResultProcessingMixin, BaseSearchBackend):
    # Reserved words are irrelevant over the wire (values travel as JSON,
    # not as query strings) but the base class expects the attribute.
    RESERVED_WORDS = frozenset()

    def __init__(self, connection_alias, **connection_options):
        super().__init__(connection_alias, **connection_options)
        self.setup_complete = False
        self.url = connection_options.get("URL", "http://127.0.0.1:8117")
        self.timeout = int(connection_options.get("TIMEOUT", 10))
        self.include_spelling = connection_options.get("INCLUDE_SPELLING", False)
        self.log = logging.getLogger("haystack")
        self.client = PitchforkClient(self.url, timeout=self.timeout)
        self.content_field_name = "text"
        self.schema_fp = None
        self.schema_definition = None

    def setup(self):
        """Defers loading until needed. Compiles the schema once and
        registers it with the server; a fingerprint mismatch afterwards is a
        hard error, never a rebuild."""
        from haystack import connections

        searchfields = connections[self.connection_alias].get_unified_index().all_searchfields()
        self.content_field_name, self.schema_definition = (
            adapter_base.build_schema_definition(searchfields)
        )

        ack = self.client.post(
            "/setup",
            {
                "v": wire_v1.WIRE_VERSION,
                "schema_fp": None,
                "schema": self.schema_definition,
                "docs": [],
            },
            expected="ack",
        )
        self.schema_fp = ack.get("schema_fp")
        if not self.schema_fp:
            raise PitchforkError(
                "pitchfork-server /setup did not return a schema fingerprint"
            )
        self.setup_complete = True

    def _require_setup(self):
        if not self.setup_complete:
            self.setup()

    # ------------------------------------------------------------------
    # Indexing
    # ------------------------------------------------------------------

    def update(self, index, iterable, commit=True):
        self._require_setup()

        docs = []
        for obj in iterable:
            try:
                doc = index.full_prepare(obj)
            except SkipDocument:
                self.log.debug("Indexing for object `%s` skipped", obj)
                continue

            prepared = {}
            for key, value in doc.items():
                if key == "boost":
                    continue
                prepared[key] = encode_wire_value(value)

            # update_document semantics: replace any existing version.
            prepared["id"] = get_identifier(obj)
            docs.append(prepared)

        if not docs:
            return

        self.client.post(
            "/index",
            {
                "v": wire_v1.WIRE_VERSION,
                "schema_fp": self.schema_fp,
                "schema": self.schema_definition,
                "docs": docs,
            },
            expected="ack",
        )

    def remove(self, obj_or_string, commit=True):
        self._require_setup()

        doc_id = get_identifier(obj_or_string)
        self.client.post(
            "/remove",
            {"v": wire_v1.WIRE_VERSION, "schema_fp": self.schema_fp, "ids": [doc_id]},
            expected="ack",
        )

    def clear(self, models=None, commit=True):
        self._require_setup()

        if models is not None:
            warnings.warn(
                "Pitchfork V1 clears the whole index; per-model clearing is "
                "not supported yet.",
                Warning,
                stacklevel=2,
            )

        self.client.post(
            "/clear",
            {"v": wire_v1.WIRE_VERSION, "schema_fp": self.schema_fp},
            expected="ack",
        )

    def delete_index(self):
        self.clear(models=None)

    def optimize(self):
        # Tantivy compaction is handled by the engine; nothing to do.
        pass

    # ------------------------------------------------------------------
    # Searching
    # ------------------------------------------------------------------

    @log_query
    def search(
        self,
        query_string,
        sort_by=None,
        start_offset=0,
        end_offset=None,
        fields="",
        highlight=False,
        facets=None,
        date_facets=None,
        query_facets=None,
        narrow_queries=None,
        spelling_query=None,
        within=None,
        dwithin=None,
        distance_point=None,
        models=None,
        limit_to_registered_models=None,
        result_class=None,
        **kwargs,
    ):
        self._require_setup()

        if distance_point is not None:
            # P1-2: accepted-but-ignored features must be loud. V1 has no
            # distance annotation support (see MIGRATION.md).
            warnings.warn(
                "Pitchfork does not compute distances yet; `distance_point` "
                "is ignored and results carry no `_distance` annotation.",
                Warning,
                stacklevel=2,
            )

        if date_facets:
            # Accepted-and-ignored features must be loud (V1 has no
            # date-facet IR op; phase 5+ work).
            warnings.warn(
                "Pitchfork does not support date facets yet; "
                "`date_facets` is ignored.",
                Warning,
                stacklevel=2,
            )

        if query_facets:
            warnings.warn(
                "Pitchfork does not support query facets yet; "
                "`query_facets` is ignored.",
                Warning,
                stacklevel=2,
            )

        # A zero-length query should return no results.
        if not query_string:
            return {"results": [], "hits": 0}

        ast = self._decode_query(query_string)
        if limit_to_registered_models is None:
            limit_to_registered_models = adapter_base.limit_to_registered_models()

        narrow = []
        if models and len(models):
            narrow.extend(adapter_base.models_filter(models))
        elif limit_to_registered_models:
            model_choices = self.build_models_list()
            if model_choices:
                # OR of django_ct over registered models.
                narrow.append(
                    {
                        "node": "or",
                        "children": [
                            {
                                "node": "filter",
                                "field": DJANGO_CT,
                                "op": "exact",
                                "value": rm,
                            }
                            for rm in model_choices
                        ],
                    }
                )

        if narrow_queries:
            warnings.warn(
                "Pitchfork does not handle raw narrow query strings yet.",
                Warning,
                stacklevel=2,
            )

        if narrow:
            ast = {"node": "and", "children": [ast] + narrow}

        request = {
            "v": wire_v1.WIRE_VERSION,
            "schema_fp": self.schema_fp,
            "ast": ast,
            "start": start_offset,
            "end": end_offset,
            "sort": [str(field) for field in (sort_by or [])],
            "facets": sorted(facets.keys()) if facets else [],
            "highlight": bool(highlight),
            "spellcheck": bool(spelling_query) or self.include_spelling,
            # Wire rule: spelling_query defaults to the main query server-side.
            "spelling_query": spelling_query or str(query_string),
            "spatial": adapter_base.spatial_filter(within=within, dwithin=dwithin),
            # P0-5: facets must be page-independent; the client picks the
            # scan cap for the full-match-set aggregation.
            "facet_scan_limit": adapter_base.FACET_SCAN_LIMIT,
        }

        response = self.client.post("/search", request)

        if response.get("kind") == 2:
            if not self.silently_fail:
                raise PitchforkError(response.get("error") or "search failed")
            self.log.error("Pitchfork search failed: %s", response.get("error"))
            return {"results": [], "hits": 0, "spelling_suggestion": None}

        return self._process_results(
            response,
            highlight=highlight,
            result_class=result_class,
        )

    def more_like_this(
        self,
        model_instance,
        additional_query_string=None,
        start_offset=0,
        end_offset=None,
        models=None,
        limit_to_registered_models=None,
        result_class=None,
        **kwargs,
    ):
        self._require_setup()

        doc_id = get_identifier(model_instance)
        request = {
            "v": wire_v1.WIRE_VERSION,
            "schema_fp": self.schema_fp,
            "ast": {"node": "match_all"},
            "start": start_offset,
            "end": end_offset,
            "sort": [],
            "facets": [],
            "highlight": False,
            "spellcheck": False,
            "mlt_doc_id": doc_id,
        }
        response = self.client.post("/more_like_this", request)

        if response.get("kind") == 2:
            if not self.silently_fail:
                raise PitchforkError(response.get("error") or "more_like_this failed")
            self.log.error("Pitchfork MLT failed: %s", response.get("error"))
            return {"results": [], "hits": 0}

        return self._process_results(response, result_class=result_class)

    def build_models_list(self):
        """Sorted list of "app.model" strings for registered models."""
        from haystack import connections

        models = connections[self.connection_alias].get_unified_index().get_indexed_models()
        return sorted(get_model_ct(model) for model in models)

    def create_spelling_suggestion(self, query_string):
        # Phase 5: FST-based spelling (tantivy has no built-in spell-check).
        return None


class PitchforkSearchQuery(BaseSearchQuery):
    def add_boost(self, term, boost_value):
        """P1-1: boost is accepted for haystack-API compatibility but the
        V1 engine has no term-boost IR op; relevance control is silently
        ignored — which must be loud, not silent."""
        warnings.warn(
            "Pitchfork does not support term boosting yet; "
            "boost(%r, %r) is ignored." % (term, boost_value),
            Warning,
            stacklevel=2,
        )
        super().add_boost(term, boost_value)

    def build_query(self):
        """Builds the final query as a JSON-encoded PitchforkWireV1 AST
        instead of a backend query string. The backend decodes it."""
        ast = build_ast(self.query_filter)
        return json.dumps(ast)

    def clean(self, query_fragment):
        """Values travel as JSON; no query-string sanitizing needed."""
        return query_fragment

    def build_query_fragment(self, field, filter_type, value):
        """Not used (build_query encodes the SQ tree directly), but provided
        for compatibility with code that calls it directly."""
        return json.dumps(adapter_base.encode_filter(field, filter_type, value))

    def _convert_datetime(self, date):
        import datetime

        if isinstance(date, datetime.datetime):
            return date
        return datetime.datetime(date.year, date.month, date.day)


class PitchforkEngine(BaseEngine):
    backend = PitchforkSearchBackend
    query = PitchforkSearchQuery
