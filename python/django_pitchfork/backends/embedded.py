"""Strategy B: embed the same Rust core in-process via PyO3.

`django_pitchfork._core` wraps pitchfork-core + pitchfork-engine compiled
into the Python process. Every engine call releases the GIL (py.allow_threads
in the Rust module), and the wire types are identical to server mode — the
protocol contract cannot drift.

The C → B seam: `AutoEngine` picks embedded mode when the Rust module
imports, server mode otherwise.
"""

import json
import os
import warnings

from django.core.exceptions import ImproperlyConfigured
from haystack.backends import BaseEngine, BaseSearchBackend, log_query
from haystack.constants import DJANGO_CT
from haystack.exceptions import SkipDocument
from haystack.utils import get_identifier

from .. import wire as wire_v1
from ..exceptions import PitchforkError
from . import base as adapter_base
from .base import encode_wire_value
from .server import PitchforkSearchQuery, ResultProcessingMixin

try:
    from django_pitchfork import _core

    HAS_CORE = True
except ImportError:  # pragma: no cover - the module is optional by design
    _core = None
    HAS_CORE = False


class PitchforkEmbeddedBackend(ResultProcessingMixin, BaseSearchBackend):
    RESERVED_WORDS = frozenset()

    def __init__(self, connection_alias, **connection_options):
        super().__init__(connection_alias, **connection_options)
        self.setup_complete = False
        self.log = None
        import logging

        self.log = logging.getLogger("haystack")
        # An explicit PATH is honored across setups (fingerprint mismatch is
        # a hard error there); without one, each setup gets a fresh data dir
        # so re-registering a swapped unified index rebuilds cleanly.
        self._explicit_path = connection_options.get("PATH")
        self.data_dir = self._explicit_path
        if self.data_dir is None:
            from tempfile import mkdtemp

            self.data_dir = mkdtemp(prefix="pitchfork_embedded_")
        self.timeout = int(connection_options.get("TIMEOUT", 10))
        self.include_spelling = connection_options.get("INCLUDE_SPELLING", False)
        self.content_field_name = "text"
        self.schema_fp = None
        self.schema_definition = None
        self.engine = None
        # Python >= 3.14 defaults to forkserver; haystack's run_tests.py
        # forces fork. Engine state (tantivy writer locks) must be
        # re-initialized post-fork — never shared across processes.
        os.register_at_fork(after_in_child=self._reinit_after_fork)

    def _reinit_after_fork(self):
        self.setup_complete = False
        self.engine = None

    def setup(self):
        """Defers loading until needed. Compiles the schema once at
        registration — never per query."""
        from haystack import connections

        if not HAS_CORE:
            raise ImproperlyConfigured(
                "django_pitchfork._core is not compiled. Build the Rust "
                "extension with `maturin develop` (or `just build`) to use "
                "embedded mode."
            )

        searchfields = connections[self.connection_alias].get_unified_index().all_searchfields()
        self.content_field_name, self.schema_definition = (
            adapter_base.build_schema_definition(searchfields)
        )

        # Fresh data dir per setup when no explicit PATH was configured.
        if self._explicit_path is None:
            from tempfile import mkdtemp

            self.data_dir = mkdtemp(prefix="pitchfork_embedded_")
        self.engine = _core.PyPitchforkEngine(self.data_dir)
        self.schema_fp = self.engine.setup(json.dumps(self.schema_definition))
        self.setup_complete = True

    def _require_setup(self):
        if not self.setup_complete:
            self.setup()

    def _request(self, ast, start=0, end=None, sort=None, facets=None,
                 highlight=False, spellcheck=False):
        return {
            "v": wire_v1.WIRE_VERSION,
            "schema_fp": self.schema_fp,
            "ast": ast,
            "start": start,
            "end": end,
            "sort": sort or [],
            "facets": facets or [],
            "highlight": bool(highlight),
            "spellcheck": bool(spellcheck),
        }

    # ------------------------------------------------------------------
    # Indexing (GIL released inside the Rust module)
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
            prepared["id"] = get_identifier(obj)
            docs.append(prepared)

        if not docs:
            return

        ack = json.loads(
            self.engine.index(
                json.dumps(
                    {
                        "v": wire_v1.WIRE_VERSION,
                        "schema_fp": self.schema_fp,
                        "schema": self.schema_definition,
                        "docs": docs,
                    }
                )
            )
        )
        if ack.get("ok") is not True:
            raise PitchforkError(
                f"embedded index failed: {ack.get('error')}"
            )

    def remove(self, obj_or_string, commit=True):
        self._require_setup()

        doc_id = get_identifier(obj_or_string)
        ack = json.loads(
            self.engine.remove(
                json.dumps(
                    {
                        "v": wire_v1.WIRE_VERSION,
                        "schema_fp": self.schema_fp,
                        "ids": [doc_id],
                    }
                )
            )
        )
        if ack.get("ok") is not True:
            raise PitchforkError(f"embedded remove failed: {ack.get('error')}")

    def clear(self, models=None, commit=True):
        self._require_setup()

        if models is not None:
            warnings.warn(
                "Pitchfork V1 clears the whole index; per-model clearing is "
                "not supported yet.",
                Warning,
                stacklevel=2,
            )

        ack = json.loads(self.engine.clear())
        if ack.get("ok") is not True:
            raise PitchforkError(f"embedded clear failed: {ack.get('error')}")

    def delete_index(self):
        self.clear(models=None)

    def optimize(self):
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

        request = self._request(
            ast,
            start=start_offset,
            end=end_offset,
            sort=[str(field) for field in (sort_by or [])],
            facets=sorted(facets.keys()) if facets else [],
            highlight=highlight,
            spellcheck=bool(spelling_query) or self.include_spelling,
        )
        # Wire rule: spelling_query defaults to the main query server-side.
        request["spelling_query"] = spelling_query or str(query_string)
        spatial = adapter_base.spatial_filter(within=within, dwithin=dwithin)
        if spatial is not None:
            request["spatial"] = spatial

        response = json.loads(self.engine.search(json.dumps(request)))

        if response.get("kind") == wire_v1.KIND_ERROR:
            if not self.silently_fail:
                raise PitchforkError(response.get("error") or "search failed")
            self.log.error("Pitchfork embedded search failed: %s", response.get("error"))
            return {"results": [], "hits": 0, "spelling_suggestion": None}

        return self._process_results(response, highlight=highlight, result_class=result_class)

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
        request = self._request(
            {"node": "match_all"},
            start=start_offset,
            end=end_offset,
        )
        response = json.loads(
            self.engine.more_like_this(doc_id, json.dumps(request))
        )

        if response.get("kind") == wire_v1.KIND_ERROR:
            if not self.silently_fail:
                raise PitchforkError(response.get("error") or "more_like_this failed")
            self.log.error("Pitchfork embedded MLT failed: %s", response.get("error"))
            return {"results": [], "hits": 0}

        return self._process_results(response, result_class=result_class)

    def build_models_list(self):
        from haystack import connections
        from haystack.utils import get_model_ct

        models = connections[self.connection_alias].get_unified_index().get_indexed_models()
        return sorted(get_model_ct(model) for model in models)

    def create_spelling_suggestion(self, query_string):
        return None


class PitchforkEmbeddedEngine(BaseEngine):
    backend = PitchforkEmbeddedBackend
    query = PitchforkSearchQuery


class AutoEngine(BaseEngine):
    """The C → B seam: embedded mode when the Rust module imports, server
    mode otherwise. Both modes speak identical wire types."""

    def __init__(self, using=None):
        from . import server

        self.backend = PitchforkEmbeddedBackend if HAS_CORE else server.PitchforkSearchBackend
        self.query = PitchforkSearchQuery if HAS_CORE else server.PitchforkSearchQuery
        super().__init__(using=using)

    def _make_backend_instance(self, connection_alias, connection_settings):
        from . import server

        if HAS_CORE:
            return PitchforkEmbeddedBackend(connection_alias, **connection_settings)
        return server.PitchforkSearchBackend(connection_alias, **connection_settings)

    def _make_query_instance(self, connection_alias, **kwargs):
        from . import server

        if HAS_CORE:
            return PitchforkSearchQuery(connection_alias, **kwargs)
        return server.PitchforkSearchQuery(connection_alias, **kwargs)
