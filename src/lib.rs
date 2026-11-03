//! PyO3 bindings for the pitchfork Rust core (Strategy B, phase 4).
//!
//! Design rules (django-bolt lessons applied):
//! - `extension-module` is NOT hardcoded; maturin enables it via
//!   `[tool.maturin] features = ["pyo3/extension-module"]`, so native Rust
//!   tests can link libpython normally.
//! - Every engine call releases the GIL (`py.allow_threads`) — search,
//!   indexing and clearing never hold the GIL.
//! - The interface is JSON in / JSON out over the same PitchforkWireV1
//!   serde types the server uses, so the protocol contract cannot drift.


use pitchfork_core::schema::PitchforkSchema;
use pitchfork_core::wire::{
    IndexRequestV1, RemoveRequestV1, SearchRequestV1, SearchResponseV1, AckResponseV1,
};
use pitchfork_engine::{SearchEngine, TantivyEngine};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

fn to_py_err<E: std::fmt::Display>(err: E) -> PyErr {
    PyValueError::new_err(err.to_string())
}

/// In-process Tantivy-backed search engine. Compile once at construction,
/// reuse forever — the schema, reader and writer are built in `setup` and
/// never rebuilt per query.
#[pyclass]
struct PyPitchforkEngine {
    inner: TantivyEngine,
}

#[pymethods]
impl PyPitchforkEngine {
    #[new]
    #[pyo3(signature = (data_dir))]
    fn new(data_dir: String) -> PyResult<PyPitchforkEngine> {
        let inner = TantivyEngine::new(data_dir).map_err(to_py_err)?;
        Ok(PyPitchforkEngine { inner })
    }

    /// Register the schema (JSON). Returns the server-computed fingerprint.
    fn setup(&self, schema_json: &str) -> PyResult<String> {
        let schema: PitchforkSchema =
            serde_json::from_str(schema_json).map_err(to_py_err)?;
        let fp = schema.fingerprint();
        self.inner.setup(&schema).map_err(to_py_err)?;
        Ok(fp)
    }

    /// Search (request JSON -> response JSON). GIL released.
    fn search(&self, py: Python<'_>, request_json: &str) -> PyResult<String> {
        let request: SearchRequestV1 = serde_json::from_str(request_json).map_err(to_py_err)?;
        let response: SearchResponseV1 =
            py.allow_threads(|| self.inner.search(&request).map_err(to_py_err))?;
        serde_json::to_string(&response).map_err(to_py_err)
    }

    /// Index documents (request JSON -> ack JSON). GIL released.
    fn index(&self, py: Python<'_>, request_json: &str) -> PyResult<String> {
        let request: IndexRequestV1 = serde_json::from_str(request_json).map_err(to_py_err)?;
        let ack: AckResponseV1 =
            py.allow_threads(|| self.inner.index(&request).map_err(to_py_err))?;
        serde_json::to_string(&ack).map_err(to_py_err)
    }

    /// Remove documents by id (request JSON -> ack JSON). GIL released.
    fn remove(&self, py: Python<'_>, request_json: &str) -> PyResult<String> {
        let request: RemoveRequestV1 = serde_json::from_str(request_json).map_err(to_py_err)?;
        let ack: AckResponseV1 =
            py.allow_threads(|| self.inner.remove(&request).map_err(to_py_err))?;
        serde_json::to_string(&ack).map_err(to_py_err)
    }

    /// Clear the index (ack JSON). GIL released.
    fn clear(&self, py: Python<'_>) -> PyResult<String> {
        // ClearRequestV1 is validated implicitly by the engine; the ack is
        // what crosses the boundary.
        let ack: AckResponseV1 = py.allow_threads(|| self.inner.clear().map_err(to_py_err))?;
        serde_json::to_string(&ack).map_err(to_py_err)
    }

    /// More-like-this by document id (response JSON). GIL released.
    fn more_like_this(&self, py: Python<'_>, doc_id: &str, request_json: &str) -> PyResult<String> {
        let mut request: SearchRequestV1 =
            serde_json::from_str(request_json).map_err(to_py_err)?;
        request.mlt_doc_id = Some(doc_id.to_string());
        let response: SearchResponseV1 = py.allow_threads(|| {
            self.inner.more_like_this(doc_id, &request).map_err(to_py_err)
        })?;
        serde_json::to_string(&response).map_err(to_py_err)
    }

    /// Current schema fingerprint, if a schema has been registered.
    fn schema_fp(&self) -> Option<String> {
        self.inner.schema_fp()
    }
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyPitchforkEngine>()?;
    m.add("WIRE_VERSION", pitchfork_core::WIRE_VERSION)?;
    Ok(())
}
