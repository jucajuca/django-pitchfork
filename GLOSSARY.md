# django-pitchfork

Rust-powered search for Django, and a complete replacement for
django-haystack. The `pitchfork` package exposes the search API; backends
range from the Rust-native Tantivy core to external engines (Solr,
Elasticsearch, Whoosh, Xapian).

## Language

**Pitchfork (package)**:
The haystack-replacement package exposing the search API, backends, and
template tags.
_Avoid_: django-pitchfork (that is the other package), the library

**django-pitchfork (package)**:
The released package containing the Rust-native engine bindings and wire
codec, wrapped by the pitchfork package.
_Avoid_: pitchfork (ambiguous with the replacement package)

**Native backend**:
The Rust/Tantivy engine that pitchfork ships itself, as opposed to adapters
to external search engines.
_Avoid_: Rust backend, built-in backend

**Embedded mode**:
The native engine running in-process via the compiled extension module.
_Avoid_: PyO3 mode, Strategy B

**Server mode**:
The native engine behind the separate pitchfork-server HTTP service.
_Avoid_: Strategy C, remote mode

**Query IR**:
The single engine-neutral query representation that every backend lowers
from; never built per query at runtime.
_Avoid_: AST (ambiguous), query tree, QueryNode (that is its Rust type)

**Lowering**:
Translating the query IR into an engine's native query DSL or parameters.
_Avoid_: compilation, query building (that is accumulating the IR)

**Conformance suite**:
The one backend-agnostic test suite that every backend must pass or declare
gaps against.
_Avoid_: backend tests, integration tests, per-backend tests

**Backend caps**:
A backend's declaration of which IR ops it supports and where its conformance
expectations differ from the universal ones.
_Avoid_: feature matrix, capability list

**Wire version**:
The versioned contract between the Python side and the native engine;
mismatch is a hard error.
_Avoid_: protocol version, API version

**Schema fingerprint**:
The identity of the compiled index schema, checked when a connection
initializes; mismatch is a hard error, never a silent rebuild.
_Avoid_: schema hash

**Connection alias**:
The settings key under which a search backend connection is registered.
_Avoid_: backend name, engine name
